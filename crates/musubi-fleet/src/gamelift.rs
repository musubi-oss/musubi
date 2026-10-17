//! Amazon GameLift as a fleet provider: a game server is a game session created on the configured
//! GameLift fleet/alias (`CreateGameSession`, SigV4-signed JSON API), polled with
//! `DescribeGameSessions` until `ACTIVE`, ended with `TerminateGameSession`.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::config::GameLiftConfig;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

pub struct GameLiftProvider {
    cfg: GameLiftConfig,
    client: reqwest::Client,
    sessions: Arc<DashMap<Uuid, String>>,
}

fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn sha256_hex(data: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&k, data).as_ref().to_vec()
}

fn amz_date(t: time::OffsetDateTime) -> (String, String) {
    let date = format!("{:04}{:02}{:02}", t.year(), t.month() as u8, t.day());
    let full = format!("{date}T{:02}{:02}{:02}Z", t.hour(), t.minute(), t.second());
    (date, full)
}

/// SigV4 `Authorization` header for a GameLift JSON call (`POST /`, `application/x-amz-json-1.1`).
pub fn sigv4_authorization(
    cfg: &GameLiftConfig,
    host: &str,
    target: &str,
    body: &str,
    now: time::OffsetDateTime,
) -> (String, String) {
    let (date, amz) = amz_date(now);
    let mut headers = vec![
        (
            "content-type".to_string(),
            "application/x-amz-json-1.1".to_string(),
        ),
        ("host".to_string(), host.to_string()),
        ("x-amz-date".to_string(), amz.clone()),
        ("x-amz-target".to_string(), target.to_string()),
    ];
    if !cfg.session_token.is_empty() {
        headers.push((
            "x-amz-security-token".to_string(),
            cfg.session_token.clone(),
        ));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed_headers = signed.join(";");
    let canonical = format!(
        "POST\n/\n\n{canonical_headers}\n{signed_headers}\n{}",
        sha256_hex(body.as_bytes())
    );
    let scope = format!("{date}/{}/gamelift/aws4_request", cfg.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let k_date = hmac(
        format!("AWS4{}", cfg.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, cfg.region.as_bytes());
    let k_service = hmac(&k_region, b"gamelift");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
    (
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            cfg.access_key
        ),
        amz,
    )
}

impl GameLiftProvider {
    pub fn new(cfg: GameLiftConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            sessions: Arc::new(DashMap::new()),
        }
    }

    fn endpoint(&self) -> String {
        if self.cfg.url.is_empty() {
            format!("https://gamelift.{}.amazonaws.com", self.cfg.region)
        } else {
            self.cfg.url.trim_end_matches('/').to_string()
        }
    }

    async fn call(&self, action: &str, body: Value) -> Result<Value> {
        let endpoint = self.endpoint();
        let host = endpoint
            .split("://")
            .nth(1)
            .unwrap_or(&endpoint)
            .trim_end_matches('/')
            .to_string();
        let target = format!("GameLift.{action}");
        let body = body.to_string();
        let (auth, amz) = sigv4_authorization(
            &self.cfg,
            &host,
            &target,
            &body,
            time::OffsetDateTime::now_utc(),
        );
        let mut req = self
            .client
            .post(format!("{endpoint}/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-date", amz)
            .header("x-amz-target", &target)
            .header("authorization", auth);
        if !self.cfg.session_token.is_empty() {
            req = req.header("x-amz-security-token", &self.cfg.session_token);
        }
        let r = req
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("gamelift {action}: {e}")))?;
        let st = r.status();
        let out: Value = r.json().await.unwrap_or(Value::Null);
        if !st.is_success() {
            return Err(Error::Unavailable(format!("gamelift {action} {st}: {out}")));
        }
        Ok(out)
    }

    fn target_field(&self) -> Result<(&'static str, &str)> {
        if !self.cfg.fleet_id.is_empty() {
            Ok(("FleetId", &self.cfg.fleet_id))
        } else if !self.cfg.alias_id.is_empty() {
            Ok(("AliasId", &self.cfg.alias_id))
        } else {
            Err(Error::InvalidArgument(
                "[fleet.gamelift] needs fleet_id or alias_id".into(),
            ))
        }
    }
}

fn session_endpoint(gs: &Value) -> Option<(String, i32)> {
    let port = gs["Port"].as_i64()?;
    let addr = gs["DnsName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or(gs["IpAddress"].as_str())?;
    Some((addr.to_string(), port as i32))
}

impl Provider for GameLiftProvider {
    fn name(&self) -> &'static str {
        "gamelift"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let (field, id) = self.target_field()?;
            let props: Vec<Value> = [
                ("MUSUBI_SERVER_ID", req.server_id.to_string()),
                ("MUSUBI_FLEET_ID", req.fleet.id.to_string()),
                ("MUSUBI_PORT", req.port.to_string()),
                ("MUSUBI_API", req.api_url.to_string()),
                ("MUSUBI_SERVER_KEY", req.server_key.to_string()),
            ]
            .into_iter()
            .chain(
                req.fleet
                    .spec
                    .env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.clone())),
            )
            .map(|(k, v)| json!({ "Key": k, "Value": v }))
            .collect();
            let mut body = json!({
                "MaximumPlayerSessionCount": self.cfg.max_players.max(1),
                "Name": format!("musubi-{}", req.fleet.name),
                "IdempotencyToken": req.server_id.simple().to_string(),
                "GameProperties": props,
            });
            body[field] = json!(id);
            let out = self.call("CreateGameSession", body).await?;
            let gs = &out["GameSession"];
            let session_id = gs["GameSessionId"]
                .as_str()
                .ok_or_else(|| {
                    Error::internal(format!("gamelift CreateGameSession without id: {out}"))
                })?
                .to_string();
            self.sessions.insert(req.server_id, session_id.clone());
            if gs["Status"] == "ACTIVE" {
                if let Some((address, port)) = session_endpoint(gs) {
                    return Ok(Started {
                        address,
                        port,
                        pid: None,
                    });
                }
            }
            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(self.cfg.ready_timeout_secs.max(5));
            loop {
                tokio::time::sleep(Duration::from_secs(self.cfg.poll_secs.max(1))).await;
                let d = self
                    .call(
                        "DescribeGameSessions",
                        json!({ "GameSessionId": session_id }),
                    )
                    .await?;
                let gs = &d["GameSessions"][0];
                match gs["Status"].as_str().unwrap_or("") {
                    "ACTIVE" => {
                        if let Some((address, port)) = session_endpoint(gs) {
                            return Ok(Started {
                                address,
                                port,
                                pid: None,
                            });
                        }
                    }
                    "TERMINATED" | "TERMINATING" | "ERROR" => {
                        self.sessions.remove(&req.server_id);
                        return Err(Error::Unavailable(format!(
                            "gamelift session {session_id} {}",
                            gs["Status"]
                        )));
                    }
                    _ => {}
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = self.terminate(&session_id).await;
                    self.sessions.remove(&req.server_id);
                    return Err(Error::Unavailable(format!(
                        "gamelift session {session_id} not active in time"
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
            if let Some((_, sid)) = self.sessions.remove(&server_id) {
                if let Err(e) = self.terminate(&sid).await {
                    tracing::warn!(server = %server_id, session = %sid, error = %e, "gamelift terminate failed");
                }
            }
        })
    }

    fn alive(&self, _server_id: Uuid) -> Option<bool> {
        None
    }
}

impl GameLiftProvider {
    async fn terminate(&self, session_id: &str) -> Result<()> {
        self.call(
            "TerminateGameSession",
            json!({ "GameSessionId": session_id, "TerminationMode": "TRIGGER_ON_PROCESS_TERMINATE" }),
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS SigV4 test-suite style check: deterministic signature for fixed inputs.
    #[test]
    fn sigv4_is_deterministic_and_well_formed() {
        let cfg = GameLiftConfig {
            region: "us-east-1".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            ..Default::default()
        };
        let t = time::OffsetDateTime::from_unix_timestamp(1_440_938_160).unwrap(); // 20150830T123600Z
        let (a1, amz) = sigv4_authorization(
            &cfg,
            "gamelift.us-east-1.amazonaws.com",
            "GameLift.CreateGameSession",
            "{}",
            t,
        );
        let (a2, _) = sigv4_authorization(
            &cfg,
            "gamelift.us-east-1.amazonaws.com",
            "GameLift.CreateGameSession",
            "{}",
            t,
        );
        assert_eq!(amz, "20150830T123600Z");
        assert_eq!(a1, a2);
        assert!(a1.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/gamelift/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-target, Signature="), "{a1}");
        assert_eq!(a1.rsplit('=').next().unwrap().len(), 64);
        let (a3, _) = sigv4_authorization(
            &cfg,
            "gamelift.us-east-1.amazonaws.com",
            "GameLift.CreateGameSession",
            "{\"x\":1}",
            t,
        );
        assert_ne!(a1, a3, "body is part of the signature");
    }
}
