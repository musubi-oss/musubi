//! In-app purchase validation: Apple App Store Server API and Google Play Developer API.
//! Both return a normalized [`StoreTransaction`]; mapping to catalog items lives in the economy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreTransaction {
    pub provider: String,
    pub transaction_id: String,
    pub product_id: String,
    pub environment: String,
    pub purchased_at_ms: i64,
    pub quantity: i64,
    pub raw: Value,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Decodes the payload segment of a JWS without signature verification. Only used on
/// responses fetched from the provider over an authenticated TLS connection.
pub fn jws_payload(jws: &str) -> Result<Value> {
    let payload = jws
        .split('.')
        .nth(1)
        .ok_or_else(|| Error::Internal("malformed JWS".into()))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| Error::Internal(format!("JWS payload: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::Internal(format!("JWS payload: {e}")))
}

pub struct AppleStoreClient {
    http: reqwest::Client,
    url: String,
    issuer_id: String,
    key_id: String,
    bundle_id: String,
    key: EncodingKey,
    /// Trust anchor for signed payloads (Apple Root CA - G3 in production).
    root: Vec<u8>,
}

impl AppleStoreClient {
    pub fn new(
        url: &str,
        issuer_id: &str,
        key_id: &str,
        bundle_id: &str,
        private_key_pem: &str,
    ) -> Result<Arc<Self>> {
        Self::new_with_root(
            url,
            issuer_id,
            key_id,
            bundle_id,
            private_key_pem,
            APPLE_ROOT_CA_G3,
        )
    }

    /// Verify any App Store signed payload (transactions, server notifications) with our anchor.
    pub fn verify_payload(&self, jws: &str) -> Result<Value> {
        verify_apple_jws(jws, &self.root, OffsetDateTime::now_utc())
    }

    /// Like [`Self::new`] with an explicit trust anchor (DER) for the `x5c` chain; tests use it.
    pub fn new_with_root(
        url: &str,
        issuer_id: &str,
        key_id: &str,
        bundle_id: &str,
        private_key_pem: &str,
        root_der: &[u8],
    ) -> Result<Arc<Self>> {
        let key = EncodingKey::from_ec_pem(private_key_pem.as_bytes())
            .map_err(|e| Error::InvalidArgument(format!("apple private key: {e}")))?;
        Ok(Arc::new(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(Error::internal)?,
            url: url.trim_end_matches('/').to_string(),
            issuer_id: issuer_id.into(),
            key_id: key_id.into(),
            bundle_id: bundle_id.into(),
            key,
            root: root_der.to_vec(),
        }))
    }

    fn auth_jwt(&self) -> Result<String> {
        let mut h = Header::new(Algorithm::ES256);
        h.kid = Some(self.key_id.clone());
        h.typ = Some("JWT".into());
        let now = now_secs();
        let claims = json!({
            "iss": self.issuer_id, "iat": now, "exp": now + 300,
            "aud": "appstoreconnect-v1", "bid": self.bundle_id,
        });
        jsonwebtoken::encode(&h, &claims, &self.key).map_err(Error::internal)
    }

    /// Looks up a StoreKit 2 transaction id and checks bundle id + revocation.
    pub async fn transaction(&self, transaction_id: &str) -> Result<StoreTransaction> {
        if transaction_id.is_empty() || transaction_id.len() > 128 {
            return Err(Error::InvalidArgument("transaction_id".into()));
        }
        let res = self
            .http
            .get(format!(
                "{}/inApps/v1/transactions/{transaction_id}",
                self.url
            ))
            .bearer_auth(self.auth_jwt()?)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("apple: {e}")))?;
        match res.status().as_u16() {
            200 => {}
            404 => return Err(Error::NotFound("apple transaction".into())),
            401 => return Err(Error::Internal("apple: rejected server credentials".into())),
            s => return Err(Error::Unavailable(format!("apple: status {s}"))),
        }
        let body: Value = res.json().await.map_err(Error::internal)?;
        let jws = body["signedTransactionInfo"]
            .as_str()
            .ok_or_else(|| Error::Internal("apple: missing signedTransactionInfo".into()))?;
        let t = verify_apple_jws(jws, &self.root, OffsetDateTime::now_utc())?;
        if t["bundleId"].as_str() != Some(self.bundle_id.as_str()) {
            return Err(Error::PermissionDenied(
                "apple: transaction belongs to another app".into(),
            ));
        }
        if !t["revocationDate"].is_null() {
            return Err(Error::PermissionDenied("apple: transaction revoked".into()));
        }
        if t["transactionId"].as_str() != Some(transaction_id) {
            return Err(Error::Internal("apple: transaction id mismatch".into()));
        }
        Ok(StoreTransaction {
            provider: "apple".into(),
            transaction_id: transaction_id.into(),
            product_id: t["productId"].as_str().unwrap_or("").into(),
            environment: t["environment"].as_str().unwrap_or("Production").into(),
            purchased_at_ms: t["purchaseDate"].as_i64().unwrap_or(0),
            quantity: t["quantity"].as_i64().unwrap_or(1).max(1),
            raw: t,
        })
    }
}

pub struct GooglePlayClient {
    http: reqwest::Client,
    url: String,
    token_url: String,
    package_name: String,
    client_email: String,
    key: EncodingKey,
    token: Mutex<Option<(String, Instant)>>,
    notifications_token: String,
}

impl GooglePlayClient {
    pub fn new(
        url: &str,
        token_url: &str,
        package_name: &str,
        client_email: &str,
        private_key_pem: &str,
    ) -> Result<Arc<Self>> {
        Self::new_with_notifications(
            url,
            token_url,
            package_name,
            client_email,
            private_key_pem,
            "",
        )
    }

    /// `notifications_token` guards the Pub/Sub push endpoint for real-time developer notifications.
    pub fn new_with_notifications(
        url: &str,
        token_url: &str,
        package_name: &str,
        client_email: &str,
        private_key_pem: &str,
        notifications_token: &str,
    ) -> Result<Arc<Self>> {
        let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .map_err(|e| Error::InvalidArgument(format!("google private key: {e}")))?;
        Ok(Arc::new(Self {
            notifications_token: notifications_token.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(Error::internal)?,
            url: url.trim_end_matches('/').to_string(),
            token_url: token_url.into(),
            package_name: package_name.into(),
            client_email: client_email.into(),
            key,
            token: Mutex::new(None),
        }))
    }

    pub fn notifications_token(&self) -> &str {
        &self.notifications_token
    }

    async fn access_token(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some((t, exp)) = guard.as_ref() {
            if *exp > Instant::now() + Duration::from_secs(60) {
                return Ok(t.clone());
            }
        }
        let now = now_secs();
        let claims = json!({
            "iss": self.client_email, "scope": "https://www.googleapis.com/auth/androidpublisher",
            "aud": self.token_url, "iat": now, "exp": now + 3600,
        });
        let assertion = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.key)
            .map_err(Error::internal)?;
        let body = format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}");
        let res = self
            .http
            .post(&self.token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("google oauth: {e}")))?;
        if !res.status().is_success() {
            return Err(Error::Internal(format!(
                "google oauth: status {}",
                res.status()
            )));
        }
        let v: Value = res.json().await.map_err(Error::internal)?;
        let token = v["access_token"]
            .as_str()
            .ok_or_else(|| Error::Internal("google oauth: no access_token".into()))?
            .to_string();
        let ttl = v["expires_in"].as_u64().unwrap_or(3600);
        *guard = Some((token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token)
    }

    /// Validates a one-time product purchase token; requires `purchaseState == 0` (purchased).
    pub async fn product_purchase(
        &self,
        product_id: &str,
        purchase_token: &str,
    ) -> Result<StoreTransaction> {
        if product_id.is_empty() || purchase_token.is_empty() || purchase_token.len() > 1024 {
            return Err(Error::InvalidArgument("product_id / purchase_token".into()));
        }
        let token = self.access_token().await?;
        let res = self
            .http
            .get(format!(
                "{}/androidpublisher/v3/applications/{}/purchases/products/{product_id}/tokens/{purchase_token}",
                self.url, self.package_name
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("google: {e}")))?;
        match res.status().as_u16() {
            200 => {}
            400 | 404 => return Err(Error::NotFound("google purchase".into())),
            401 | 403 => {
                return Err(Error::Internal(
                    "google: rejected server credentials".into(),
                ))
            }
            s => return Err(Error::Unavailable(format!("google: status {s}"))),
        }
        let v: Value = res.json().await.map_err(Error::internal)?;
        let state = v["purchaseState"].as_i64().unwrap_or(-1);
        if state != 0 {
            return Err(Error::PermissionDenied(format!(
                "google: purchase not completed (state {state})"
            )));
        }
        let order_id = v["orderId"].as_str().unwrap_or(purchase_token).to_string();
        Ok(StoreTransaction {
            provider: "google".into(),
            transaction_id: order_id,
            product_id: product_id.into(),
            environment: if v["purchaseType"].as_i64() == Some(0) {
                "Test".into()
            } else {
                "Production".into()
            },
            purchased_at_ms: v["purchaseTimeMillis"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            quantity: v["quantity"].as_i64().unwrap_or(1).max(1),
            raw: {
                let mut v = v;
                v["purchaseToken"] = json!(purchase_token);
                v
            },
        })
    }
}

/// Apple Root CA - G3 (DER), the trust anchor for App Store Server API signed payloads.
pub const APPLE_ROOT_CA_G3: &[u8] = include_bytes!("../certs/AppleRootCA-G3.der");
/// Extension Apple places on App Store receipt/transaction signing certificates.
const APPLE_RECEIPT_SIGNING_OID: &str = "1.2.840.113635.100.6.11.1";

fn denied(what: &str) -> Error {
    Error::PermissionDenied(format!("apple: {what}"))
}

/// Verify an App Store Server API JWS (`signedTransactionInfo`, notifications): the `x5c` chain
/// must lead to `root_der`, every certificate must be valid at `now`, the leaf must carry Apple's
/// receipt-signing extension, and the ES256 signature must verify with the leaf key.
pub fn verify_apple_jws(jws: &str, root_der: &[u8], now: OffsetDateTime) -> Result<Value> {
    use x509_parser::prelude::*;
    let parts: Vec<&str> = jws.split('.').collect();
    if parts.len() != 3 {
        return Err(denied("malformed JWS"));
    }
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header: Value = b64
        .decode(parts[0])
        .ok()
        .and_then(|h| serde_json::from_slice(&h).ok())
        .ok_or_else(|| denied("bad JWS header"))?;
    if header["alg"].as_str() != Some("ES256") {
        return Err(denied("unexpected JWS algorithm"));
    }
    let x5c = header["x5c"]
        .as_array()
        .filter(|a| a.len() >= 2)
        .ok_or_else(|| denied("x5c chain missing"))?;
    let ders: Vec<Vec<u8>> = x5c
        .iter()
        .map(|c| {
            base64::engine::general_purpose::STANDARD
                .decode(c.as_str().unwrap_or(""))
                .map_err(|_| denied("bad x5c encoding"))
        })
        .collect::<Result<_>>()?;
    let certs: Vec<X509Certificate> = ders
        .iter()
        .map(|d| {
            X509Certificate::from_der(d)
                .map(|(_, c)| c)
                .map_err(|_| denied("bad x5c certificate"))
        })
        .collect::<Result<_>>()?;
    let (_, root) = X509Certificate::from_der(root_der)
        .map_err(|_| Error::Internal("apple root cert".into()))?;
    let at = ASN1Time::from_timestamp(now.unix_timestamp())
        .map_err(|_| Error::Internal("time".into()))?;
    for (i, c) in certs.iter().enumerate() {
        if !c.validity().is_valid_at(at) {
            return Err(denied("certificate not valid at this time"));
        }
        if i > 0 && !c.is_ca() {
            return Err(denied("intermediate is not a CA"));
        }
        let last = i + 1 == certs.len();
        if last && ders[i] == root_der {
            continue;
        }
        let issuer = if last { &root } else { &certs[i + 1] };
        c.verify_signature(Some(issuer.public_key()))
            .map_err(|_| denied("certificate chain does not lead to Apple Root CA"))?;
    }
    let leaf = &certs[0];
    if !leaf
        .extensions()
        .iter()
        .any(|e| e.oid.to_id_string() == APPLE_RECEIPT_SIGNING_OID)
    {
        return Err(denied("leaf is not an App Store signing certificate"));
    }
    let sig = b64
        .decode(parts[2])
        .map_err(|_| denied("bad signature encoding"))?;
    let spki = leaf.public_key().subject_public_key.data.as_ref();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, spki)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .map_err(|_| denied("JWS signature invalid"))?;
    let payload = b64
        .decode(parts[1])
        .map_err(|_| denied("bad payload encoding"))?;
    serde_json::from_slice(&payload).map_err(|_| denied("bad payload"))
}

// ----- Steam microtransactions (ISteamMicroTxn) -----

#[derive(Debug, Clone)]
pub struct SteamInitTxn {
    pub order_id: i64,
    pub steam_id: String,
    pub item_id: String,
    pub quantity: i64,
    pub amount_cents: i64,
    pub description: String,
    pub language: String,
    pub currency: String,
}

/// Steamworks Web API microtransactions: InitTxn → (user authorizes in the overlay) →
/// FinalizeTxn captures funds; QueryTxn reports refunds/chargebacks for reconciliation.
pub struct SteamClient {
    http: reqwest::Client,
    url: String,
    key: String,
    app_id: u32,
    sandbox: bool,
}

impl SteamClient {
    pub fn new(url: &str, key: &str, app_id: u32, sandbox: bool) -> Result<Arc<Self>> {
        if key.is_empty() {
            return Err(Error::InvalidArgument("steam web api key".into()));
        }
        Ok(Arc::new(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(Error::internal)?,
            url: url.trim_end_matches('/').to_string(),
            key: key.into(),
            app_id,
            sandbox,
        }))
    }

    pub fn sandbox(&self) -> bool {
        self.sandbox
    }

    fn endpoint(&self, method: &str, version: u8) -> String {
        let iface = if self.sandbox {
            "ISteamMicroTxnSandbox"
        } else {
            "ISteamMicroTxn"
        };
        format!("{}/{iface}/{method}/v{version}/", self.url)
    }

    fn params(v: Value) -> Result<Value> {
        let r = &v["response"];
        if r["result"].as_str() != Some("OK") {
            let desc = r["error"]["errordesc"]
                .as_str()
                .or(r["params"]["error"]["errordesc"].as_str())
                .unwrap_or("steam: request failed");
            return Err(Error::PermissionDenied(format!("steam: {desc}")));
        }
        Ok(r["params"].clone())
    }

    async fn post(&self, method: &str, version: u8, form: &[(&str, String)]) -> Result<Value> {
        let res = self
            .http
            .post(self.endpoint(method, version))
            .form(form)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("steam: {e}")))?;
        if !res.status().is_success() {
            return Err(Error::Unavailable(format!(
                "steam: status {}",
                res.status()
            )));
        }
        Self::params(res.json().await.map_err(Error::internal)?)
    }

    /// Starts an order; returns Steam's transaction id. The user must then authorize it.
    pub async fn init_txn(&self, t: &SteamInitTxn) -> Result<String> {
        let p = self
            .post(
                "InitTxn",
                3,
                &[
                    ("key", self.key.clone()),
                    ("orderid", t.order_id.to_string()),
                    ("steamid", t.steam_id.clone()),
                    ("appid", self.app_id.to_string()),
                    ("itemcount", "1".into()),
                    ("language", t.language.clone()),
                    ("currency", t.currency.clone()),
                    ("itemid[0]", t.item_id.clone()),
                    ("qty[0]", t.quantity.to_string()),
                    ("amount[0]", (t.amount_cents * t.quantity).to_string()),
                    ("description[0]", t.description.chars().take(128).collect()),
                ],
            )
            .await?;
        p["transid"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::Internal("steam: no transid".into()))
    }

    /// Captures funds for an authorized order; a success means the items may be granted.
    pub async fn finalize_txn(&self, order_id: i64) -> Result<Value> {
        self.post(
            "FinalizeTxn",
            2,
            &[
                ("key", self.key.clone()),
                ("orderid", order_id.to_string()),
                ("appid", self.app_id.to_string()),
            ],
        )
        .await
    }

    /// Order status (`Succeeded`, `Refunded`, `Chargedback`, `PartialRefund`, ...).
    pub async fn query_txn(&self, order_id: i64) -> Result<Value> {
        let res = self
            .http
            .get(self.endpoint("QueryTxn", 3))
            .query(&[
                ("key", self.key.clone()),
                ("appid", self.app_id.to_string()),
                ("orderid", order_id.to_string()),
            ])
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("steam: {e}")))?;
        if !res.status().is_success() {
            return Err(Error::Unavailable(format!(
                "steam: status {}",
                res.status()
            )));
        }
        Self::params(res.json().await.map_err(Error::internal)?)
    }
}
