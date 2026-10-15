//! Mobile push: FCM HTTP v1 (service-account OAuth) and APNs (token auth). A notification that
//! reaches no live session of the account is pushed to every registered device token; tokens the
//! provider reports as gone are deleted.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use musubi_core::config::PushConfig;
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;

pub struct Push {
    cfg: PushConfig,
    db: PgPool,
    http: reqwest::Client,
    fcm_token: Mutex<Option<(String, Instant)>>,
}

impl Push {
    pub fn new(cfg: PushConfig, db: PgPool) -> Self {
        Self {
            cfg,
            db,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            fcm_token: Mutex::new(None),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.cfg.fcm.project_id.is_empty() || !self.cfg.apns.key_id.is_empty()
    }

    pub async fn register(&self, account: AccountId, platform: &str, token: &str) -> Result<()> {
        if !matches!(platform, "fcm" | "apns") {
            return Err(Error::InvalidArgument(
                "platform must be fcm or apns".into(),
            ));
        }
        if token.is_empty() || token.len() > 4096 {
            return Err(Error::InvalidArgument(
                "token must be 1..=4096 chars".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.push_tokens (platform, token, account_id) values ($1, $2, $3)
             on conflict (platform, token) do update set account_id = excluded.account_id, created_at = now()",
        )
        .bind(platform)
        .bind(token)
        .bind(account.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn unregister(&self, account: AccountId, token: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.push_tokens where token = $1 and account_id = $2")
            .bind(token)
            .bind(account.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }

    pub async fn list(&self, account: AccountId) -> Result<Vec<Value>> {
        let rows: Vec<(String, String, time::OffsetDateTime)> = sqlx::query_as(
            "select platform, token, created_at from musubi.push_tokens where account_id = $1 order by created_at",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(p, t, c)| json!({ "platform": p, "token": t, "created_at": c.unix_timestamp() }))
            .collect())
    }

    /// Push one notification to every device of the account; returns deliveries attempted.
    pub async fn send(
        &self,
        account: AccountId,
        title: &str,
        data: &Value,
        code: i32,
    ) -> Result<usize> {
        if !self.enabled() {
            return Ok(0);
        }
        let rows: Vec<(String, String)> =
            sqlx::query_as("select platform, token from musubi.push_tokens where account_id = $1")
                .bind(account.0)
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        let mut sent = 0;
        for (platform, token) in rows {
            let res = match platform.as_str() {
                "fcm" if !self.cfg.fcm.project_id.is_empty() => {
                    self.fcm(&token, title, data, code).await
                }
                "apns" if !self.cfg.apns.key_id.is_empty() => {
                    self.apns(&token, title, data, code).await
                }
                _ => continue,
            };
            match res {
                Ok(true) => sent += 1,
                Ok(false) => {
                    tracing::info!(platform, "push token gone, removing");
                    let _ = sqlx::query(
                        "delete from musubi.push_tokens where platform = $1 and token = $2",
                    )
                    .bind(&platform)
                    .bind(&token)
                    .execute(&self.db)
                    .await;
                }
                Err(e) => tracing::warn!(platform, error = %e, "push failed"),
            }
        }
        Ok(sent)
    }

    async fn fcm_access_token(&self) -> Result<String> {
        if let Some((t, until)) = self.fcm_token.lock().unwrap().clone() {
            if Instant::now() < until {
                return Ok(t);
            }
        }
        let c = &self.cfg.fcm;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let claims = json!({
            "iss": c.client_email, "sub": c.client_email, "aud": c.token_url,
            "scope": "https://www.googleapis.com/auth/firebase.messaging",
            "iat": now, "exp": now + 3600,
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(c.private_key_pem.as_bytes())
            .map_err(|e| Error::Internal(format!("fcm private key: {e}")))?;
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(Error::internal)?;
        let v: Value = self
            .http
            .post(&c.token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Error::Internal(format!("fcm oauth: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("fcm oauth: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("fcm oauth: {e}")))?;
        let token = v["access_token"]
            .as_str()
            .ok_or_else(|| Error::Internal("fcm oauth: no access_token".into()))?
            .to_string();
        let ttl = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
        *self.fcm_token.lock().unwrap() =
            Some((token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token)
    }

    /// Ok(false) = token is gone (UNREGISTERED / 404).
    async fn fcm(&self, token: &str, title: &str, data: &Value, code: i32) -> Result<bool> {
        let access = self.fcm_access_token().await?;
        let mut strings = serde_json::Map::new();
        strings.insert("code".into(), Value::String(code.to_string()));
        if let Some(obj) = data.as_object() {
            for (k, v) in obj {
                strings.insert(
                    k.clone(),
                    Value::String(
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    ),
                );
            }
        }
        let url = format!(
            "{}/v1/projects/{}/messages:send",
            self.cfg.fcm.base_url.trim_end_matches('/'),
            self.cfg.fcm.project_id
        );
        let res = self
            .http
            .post(url)
            .bearer_auth(access)
            .json(&json!({ "message": { "token": token, "notification": { "title": title }, "data": strings } }))
            .send()
            .await
            .map_err(|e| Error::Internal(format!("fcm send: {e}")))?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(true);
        }
        if status.as_u16() == 404 || body.contains("UNREGISTERED") {
            return Ok(false);
        }
        Err(Error::Internal(format!("fcm send: {status} {body}")))
    }

    /// Ok(false) = token is gone (410 / BadDeviceToken).
    async fn apns(&self, token: &str, title: &str, data: &Value, code: i32) -> Result<bool> {
        let c = &self.cfg.apns;
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(c.key_id.clone());
        let claims =
            json!({ "iss": c.team_id, "iat": time::OffsetDateTime::now_utc().unix_timestamp() });
        let key = jsonwebtoken::EncodingKey::from_ec_pem(c.private_key_pem.as_bytes())
            .map_err(|e| Error::Internal(format!("apns private key: {e}")))?;
        let jwt = jsonwebtoken::encode(&header, &claims, &key).map_err(Error::internal)?;
        let url = format!("{}/3/device/{token}", c.base_url.trim_end_matches('/'));
        let res = self
            .http
            .post(url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &c.bundle_id)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(&json!({ "aps": { "alert": { "title": title } }, "code": code, "data": data }))
            .send()
            .await
            .map_err(|e| Error::Internal(format!("apns send: {e}")))?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(true);
        }
        if status.as_u16() == 410
            || body.contains("BadDeviceToken")
            || body.contains("Unregistered")
        {
            return Ok(false);
        }
        Err(Error::Internal(format!("apns send: {status} {body}")))
    }
}
