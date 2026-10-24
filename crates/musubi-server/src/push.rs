//! Push: FCM HTTP v1 (service-account OAuth), APNs (token auth) and Web Push (RFC 8030 + 8291
//! aes128gcm, VAPID). A notification that
//! reaches no live session of the account is pushed to every registered device token; tokens the
//! provider reports as gone are deleted.

use std::sync::{Mutex, OnceLock};

use base64::Engine;
use ring::rand::SecureRandom;
use ring::signature::KeyPair;
use ring::{aead, agreement, hkdf, signature};
use std::time::{Duration, Instant};

use musubi_core::config::{PushConfig, WebPushConfig};
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;

pub struct Push {
    cfg: PushConfig,
    db: PgPool,
    http: reqwest::Client,
    fcm_token: Mutex<Option<(String, Instant)>>,
    hms_token: Mutex<Option<(String, Instant)>>,
    webpush: OnceLock<WebPushKeys>,
}

pub struct WebPushKeys {
    key: signature::EcdsaKeyPair,
    public: Vec<u8>,
    subject: String,
    ttl_secs: u64,
}

struct Subscription {
    endpoint: String,
    p256dh: Vec<u8>,
    auth: Vec<u8>,
}

impl Push {
    pub fn new(cfg: PushConfig, db: PgPool) -> Self {
        let p = Self {
            cfg,
            db,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            fcm_token: Mutex::new(None),
            hms_token: Mutex::new(None),
            webpush: OnceLock::new(),
        };
        if !p.cfg.webpush.vapid_private_key_pem.is_empty() {
            if let Err(e) = p.set_webpush(&p.cfg.webpush.clone()) {
                tracing::error!(error = %e, "webpush disabled");
            }
        }
        p
    }

    /// Install VAPID keys (also used by tests to enable Web Push after construction).
    pub fn set_webpush(&self, cfg: &WebPushConfig) -> Result<()> {
        let pkcs8 = pem_der(&cfg.vapid_private_key_pem)?;
        let rng = ring::rand::SystemRandom::new();
        let key = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &pkcs8,
            &rng,
        )
        .map_err(|_| {
            Error::InvalidArgument(
                "push.webpush.vapid_private_key_pem: not a PKCS#8 P-256 private key".into(),
            )
        })?;
        let public = key.public_key().as_ref().to_vec();
        self.webpush
            .set(WebPushKeys {
                key,
                public,
                subject: cfg.subject.clone(),
                ttl_secs: cfg.ttl_secs,
            })
            .map_err(|_| Error::Conflict("webpush keys already set".into()))
    }

    /// base64url VAPID public key for `PushManager.subscribe({applicationServerKey})`.
    pub fn vapid_public_key(&self) -> Option<String> {
        self.webpush
            .get()
            .map(|k| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&k.public))
    }

    pub fn enabled(&self) -> bool {
        !self.cfg.fcm.project_id.is_empty()
            || !self.cfg.apns.key_id.is_empty()
            || !self.cfg.hms.app_id.is_empty()
            || self.webpush.get().is_some()
    }

    pub async fn register(&self, account: AccountId, platform: &str, token: &str) -> Result<()> {
        if !matches!(platform, "fcm" | "apns" | "webpush" | "hms") {
            return Err(Error::InvalidArgument(
                "platform must be fcm, apns, webpush or hms".into(),
            ));
        }
        if platform == "webpush" {
            parse_subscription(token)?;
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
                "hms" if !self.cfg.hms.app_id.is_empty() => {
                    self.hms(&token, title, data, code).await
                }
                "webpush" => match self.webpush.get() {
                    Some(k) => self.webpush(k, &token, title, data, code).await,
                    None => continue,
                },
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
            .json(&if silent(data) {
                json!({ "message": { "token": token, "data": strings,
                    "android": { "priority": "high" },
                    "apns": { "headers": { "apns-push-type": "background", "apns-priority": "5" },
                              "payload": { "aps": { "content-available": 1 } } } } })
            } else {
                let mut m = json!({ "message": { "token": token, "notification": { "title": title }, "data": strings } });
                if let Some(l) = link(data) {
                    if is_http(l) {
                        m["message"]["webpush"] = json!({ "fcm_options": { "link": l } });
                    } else {
                        m["message"]["android"] = json!({ "notification": { "click_action": l } });
                    }
                }
                m
            })
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
            .header(
                "apns-push-type",
                if silent(data) { "background" } else { "alert" },
            )
            .header("apns-priority", if silent(data) { "5" } else { "10" })
            .json(&if silent(data) {
                json!({ "aps": { "content-available": 1 }, "code": code, "data": data })
            } else {
                let mut m =
                    json!({ "aps": { "alert": { "title": title } }, "code": code, "data": data });
                if let Some(l) = link(data) {
                    m["link"] = json!(l);
                    if is_http(l) {
                        m["aps"]["url-args"] = json!([l]);
                    }
                }
                m
            })
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
    async fn hms_access_token(&self) -> Result<String> {
        if let Some((t, until)) = self.hms_token.lock().unwrap().clone() {
            if Instant::now() < until {
                return Ok(t);
            }
        }
        let c = &self.cfg.hms;
        let res = self
            .http
            .post(&c.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", c.app_id.as_str()),
                ("client_secret", c.app_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Error::Internal(format!("hms token: {e}")))?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        let Some(t) = v["access_token"].as_str() else {
            return Err(Error::Internal(format!("hms token: {status} {v}")));
        };
        let ttl = v["expires_in"]
            .as_u64()
            .unwrap_or(3600)
            .saturating_sub(60)
            .max(60);
        *self.hms_token.lock().unwrap() = Some((
            t.to_string(),
            Instant::now() + std::time::Duration::from_secs(ttl),
        ));
        Ok(t.to_string())
    }

    /// Huawei Push Kit. Ok(false) = token is gone (80300007 / 80200003 illegal token).
    async fn hms(&self, token: &str, title: &str, data: &Value, code: i32) -> Result<bool> {
        let access = self.hms_access_token().await?;
        let mut payload = data.clone();
        if !payload.is_object() {
            payload = json!({});
        }
        payload["code"] = json!(code);
        let message = if silent(data) {
            json!({ "data": payload.to_string(), "token": [token] })
        } else {
            json!({
                "data": payload.to_string(),
                "notification": { "title": title, "body": data["body"].as_str().unwrap_or("") },
                "android": { "notification": { "title": title, "body": data["body"].as_str().unwrap_or(""),
                    "click_action": match link(data) {
                        Some(l) if is_http(l) => json!({ "type": 2, "url": l }),
                        Some(l) => json!({ "type": 1, "intent": l }),
                        None => json!({ "type": 3 }),
                    } } },
                "token": [token],
            })
        };
        let url = format!(
            "{}/v1/{}/messages:send",
            self.cfg.hms.base_url.trim_end_matches('/'),
            self.cfg.hms.app_id
        );
        let res = self
            .http
            .post(url)
            .bearer_auth(access)
            .json(&json!({ "validate_only": false, "message": message }))
            .send()
            .await
            .map_err(|e| Error::Internal(format!("hms send: {e}")))?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let rc = v["code"].as_str().unwrap_or("");
        if status.is_success() && (rc == "80000000" || rc.is_empty()) {
            return Ok(true);
        }
        if matches!(rc, "80300007" | "80200003" | "80100003") || status.as_u16() == 404 {
            return Ok(false);
        }
        Err(Error::Internal(format!("hms send: {status} {body}")))
    }

    /// Ok(false) = subscription is gone (404 / 410).
    async fn webpush(
        &self,
        k: &WebPushKeys,
        token: &str,
        title: &str,
        data: &Value,
        code: i32,
    ) -> Result<bool> {
        let sub = parse_subscription(token)?;
        let mut payload = json!({ "title": title, "code": code, "data": data });
        if let Some(l) = link(data) {
            payload["url"] = json!(l);
        }
        let payload = payload.to_string();
        if payload.len() > 3_900 {
            return Err(Error::InvalidArgument(
                "webpush payload must stay under 3900 bytes".into(),
            ));
        }
        let body = encrypt(&sub, payload.as_bytes())?;
        let auth = vapid(k, &sub.endpoint)?;
        let res = self
            .http
            .post(&sub.endpoint)
            .header("authorization", auth)
            .header("content-encoding", "aes128gcm")
            .header("content-type", "application/octet-stream")
            .header("ttl", k.ttl_secs.to_string())
            .header("urgency", "normal")
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("webpush send: {e}")))?;
        let status = res.status();
        if status.is_success() {
            return Ok(true);
        }
        if matches!(status.as_u16(), 404 | 410) {
            return Ok(false);
        }
        let body = res.text().await.unwrap_or_default();
        Err(Error::Internal(format!("webpush send: {status} {body}")))
    }
}

fn pem_der(pem: &str) -> Result<Vec<u8>> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|e| Error::InvalidArgument(format!("vapid key pem: {e}")))
}

fn b64url(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|e| Error::InvalidArgument(format!("webpush key: {e}")))
}

/// Token = the browser's `PushSubscription.toJSON()`: endpoint + p256dh (65-byte point) + auth (16).
fn parse_subscription(token: &str) -> Result<Subscription> {
    let v: Value = serde_json::from_str(token).map_err(|_| {
        Error::InvalidArgument("webpush token must be the PushSubscription JSON".into())
    })?;
    let endpoint = v["endpoint"].as_str().unwrap_or("");
    check_endpoint(endpoint)?;
    let key = |k: &str| -> Result<Vec<u8>> {
        v["keys"][k]
            .as_str()
            .ok_or_else(|| {
                Error::InvalidArgument(format!("webpush subscription: keys.{k} missing"))
            })
            .and_then(b64url)
    };
    let p256dh = key("p256dh")?;
    let auth = key("auth")?;
    if p256dh.len() != 65 || p256dh[0] != 4 {
        return Err(Error::InvalidArgument(
            "webpush subscription: p256dh must be an uncompressed P-256 point".into(),
        ));
    }
    if auth.len() != 16 {
        return Err(Error::InvalidArgument(
            "webpush subscription: auth must be 16 bytes".into(),
        ));
    }
    Ok(Subscription {
        endpoint: endpoint.to_string(),
        p256dh,
        auth,
    })
}

/// Where a push service may live. The subscription comes from the player, so the endpoint is
/// attacker-controlled and the node POSTs to it (SSRF surface): only `https://` is accepted —
/// except plain `http://` to a loopback address (local push mocks) — and literal addresses in
/// private, link-local (cloud metadata), unspecified or multicast ranges are refused outright.
fn check_endpoint(endpoint: &str) -> Result<()> {
    let bad = |m: &str| Error::InvalidArgument(format!("webpush subscription endpoint: {m}"));
    if endpoint.len() > 2048 {
        return Err(bad("at most 2048 chars"));
    }
    let url = reqwest::Url::parse(endpoint).map_err(|_| bad("not a valid URL"))?;
    let scheme = url.scheme();
    if scheme != "https" && scheme != "http" {
        return Err(bad("must be https://"));
    }
    let host = url.host_str().ok_or_else(|| bad("host required"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip: Option<std::net::IpAddr> = host.parse().ok().or_else(|| {
        host.eq_ignore_ascii_case("localhost")
            .then_some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    });
    let loopback = ip.is_some_and(|ip| ip.is_loopback());
    if let Some(ip) = ip {
        if !loopback && !is_public_ip(ip) {
            return Err(bad("private, link-local or reserved address"));
        }
    }
    if scheme == "http" && !loopback {
        return Err(bad("must be https://"));
    }
    if url.username() != "" || url.password().is_some() {
        return Err(bad("credentials are not allowed"));
    }
    Ok(())
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_private()
                || a.is_link_local()
                || a.is_loopback()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || o[0] >= 240)
        }
        std::net::IpAddr::V6(a) => {
            let s = a.segments();
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public_ip(std::net::IpAddr::V4(v4));
            }
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x0064 && s[1] == 0xff9b))
        }
    }
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[u8], n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    prk.expand(&[info], Len(n))
        .and_then(|okm| okm.fill(&mut out))
        .expect("hkdf expand");
    out
}

/// RFC 8291 §3 key derivation + RFC 8188 single-record aes128gcm body (salt | rs | idlen | as_public | ciphertext).
pub(crate) fn encrypt_record(
    ua_public: &[u8],
    auth: &[u8],
    as_public: &[u8],
    ecdh_secret: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(ecdh_secret);
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    let ikm = expand(&prk_key, &info, 32);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let cek = expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = expand(&prk, b"Content-Encoding: nonce\0", 12);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &cek)
            .map_err(|_| Error::Internal("webpush: cek".into()))?,
    );
    let mut record = plaintext.to_vec();
    record.push(2);
    key.seal_in_place_append_tag(
        aead::Nonce::try_assume_unique_for_key(&nonce)
            .map_err(|_| Error::Internal("webpush: nonce".into()))?,
        aead::Aad::empty(),
        &mut record,
    )
    .map_err(|_| Error::Internal("webpush: seal".into()))?;
    let mut out = Vec::with_capacity(21 + as_public.len() + record.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&4096u32.to_be_bytes());
    out.push(as_public.len() as u8);
    out.extend_from_slice(as_public);
    out.extend_from_slice(&record);
    Ok(out)
}

fn encrypt(sub: &Subscription, plaintext: &[u8]) -> Result<Vec<u8>> {
    let rng = ring::rand::SystemRandom::new();
    let as_private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(|_| Error::Internal("webpush: keygen".into()))?;
    let as_public = as_private
        .compute_public_key()
        .map_err(|_| Error::Internal("webpush: keygen".into()))?
        .as_ref()
        .to_vec();
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &sub.p256dh);
    let secret = agreement::agree_ephemeral(as_private, &peer, |s| s.to_vec())
        .map_err(|_| Error::InvalidArgument("webpush: bad p256dh key".into()))?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt)
        .map_err(|_| Error::Internal("webpush: salt".into()))?;
    encrypt_record(
        &sub.p256dh,
        &sub.auth,
        &as_public,
        &secret,
        &salt,
        plaintext,
    )
}

/// `Authorization: vapid t=<ES256 JWT aud=origin, exp=+12h, sub>, k=<public key>` (RFC 8292).
fn vapid(k: &WebPushKeys, endpoint: &str) -> Result<String> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|e| Error::InvalidArgument(format!("webpush endpoint: {e}")))?;
    let aud = url.origin().ascii_serialization();
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 12 * 3600;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = b64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = b64.encode(json!({ "aud": aud, "exp": exp, "sub": k.subject }).to_string());
    let input = format!("{header}.{claims}");
    let rng = ring::rand::SystemRandom::new();
    let sig = k
        .key
        .sign(&rng, input.as_bytes())
        .map_err(|_| Error::Internal("webpush: vapid sign".into()))?;
    Ok(format!(
        "vapid t={input}.{}, k={}",
        b64.encode(sig.as_ref()),
        b64.encode(&k.public)
    ))
}

/// `data.link` (or `data.deep_link`): where a tap should land — an `https://` page or an app
/// scheme / intent (`mygame://shop/offer/7`). FCM: web `fcm_options.link`, otherwise Android
/// `click_action`; APNs: top-level `link` (+ `url-args` for Safari web pushes); HMS:
/// `click_action` type 2 (URL) / 1 (intent); Web Push: `url` for the service worker. The raw
/// value stays in `data` for the app either way.
fn link(data: &Value) -> Option<&str> {
    data.get("link")
        .or_else(|| data.get("deep_link"))
        .and_then(Value::as_str)
        .filter(|l| !l.is_empty() && l.len() <= 2048)
}

fn is_http(l: &str) -> bool {
    l.starts_with("https://") || l.starts_with("http://")
}

/// `data.silent == true`: deliver as a background/data-only push — no banner, the app wakes
/// and reads `data` (FCM data message, APNs `content-available`, HMS data message).
fn silent(data: &Value) -> bool {
    data.get("silent") == Some(&Value::Bool(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Vec<u8> {
        b64url(s).unwrap()
    }

    /// RFC 8291 §5 worked example.
    #[test]
    fn rfc8291_vector() {
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let as_public = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ecdh_secret = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_record(
            &ua_public,
            &auth,
            &as_public,
            &ecdh_secret,
            &salt,
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        let expect = d("DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN");
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&body),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&expect)
        );
    }

    #[test]
    fn subscription_parsing() {
        let good = json!({"endpoint": "https://push.example/abc", "keys": {"p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4", "auth": "BTBZMqHH6r4Tts7J_aSIgg"}});
        assert!(parse_subscription(&good.to_string()).is_ok());
        assert!(parse_subscription("not json").is_err());
        let mut bad = good.clone();
        bad["keys"]["auth"] = json!("c2hvcnQ");
        assert!(parse_subscription(&bad.to_string()).is_err());
        let mut bad = good.clone();
        bad["endpoint"] = json!("ftp://x");
        assert!(parse_subscription(&bad.to_string()).is_err());
    }

    /// The endpoint is player-supplied and the node POSTs to it: no plain http to the outside,
    /// no private / link-local / metadata / loopback-by-ip-literal tricks; loopback stays
    /// available for local push mocks.
    #[test]
    fn endpoint_policy_blocks_ssrf_targets() {
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            "http://127.0.0.1:8080/ok",
            "http://localhost:9000/mock",
            "http://[::1]:7000/mock",
            "https://8.8.8.8/x",
        ] {
            assert!(check_endpoint(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://169.254.169.254/latest/meta-data/",
            "https://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/admin",
            "https://10.0.0.5/admin",
            "https://172.16.3.4/",
            "https://192.168.1.1/",
            "https://100.64.0.1/",
            "https://0.0.0.0/",
            "https://[fd00::1]/",
            "https://[fe80::1]/",
            "https://[::ffff:10.0.0.1]/",
            "http://push.example.com/plain",
            "https://user:pw@push.example.com/",
            "ftp://push.example.com/",
            "https://",
            "not a url",
        ] {
            assert!(check_endpoint(bad).is_err(), "{bad}");
        }
    }
}
