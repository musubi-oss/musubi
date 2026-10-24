//! Console / launcher stores with server-to-server entitlement lookup: PlayStation Network
//! (Entitlement Web API), Xbox / Microsoft Store (Collections API), Nintendo (consumable service
//! items) and Epic Games Store (Ecom). All four follow the same shape — client-credentials OAuth
//! token, "what does this user own" query, optional consume of a consumable — so one client
//! covers them with per-platform defaults and response parsing. Paths are templates so a title
//! can follow a platform API revision (or an NDA'd endpoint) from config.

use std::time::{Duration, Instant};

use musubi_core::config::ConsoleStoreConfig;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::iap::StoreTransaction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolePlatform {
    Psn,
    Xbox,
    Nintendo,
    Epic,
}

impl ConsolePlatform {
    pub const ALL: [ConsolePlatform; 4] = [Self::Psn, Self::Xbox, Self::Nintendo, Self::Epic];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Psn => "psn",
            Self::Xbox => "xbox",
            Self::Nintendo => "nintendo",
            Self::Epic => "epic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// One owned thing on the platform store, normalized across providers.
#[derive(Debug, Clone)]
pub struct Entitlement {
    pub id: String,
    pub product_id: String,
    pub active: bool,
    pub consumable: bool,
    /// Remaining uses for consumables, 1 for durables.
    pub quantity: i64,
    pub acquired_at_ms: i64,
    pub raw: Value,
}

pub struct ConsoleStoreClient {
    platform: ConsolePlatform,
    http: reqwest::Client,
    cfg: ConsoleStoreConfig,
    token: Mutex<Option<(String, Instant)>>,
}

fn or_default(v: &str, d: &str) -> String {
    if v.is_empty() {
        d.to_string()
    } else {
        v.to_string()
    }
}

impl ConsoleStoreClient {
    /// `None` when the platform is not configured (empty `client_id`, or no `url` for Nintendo).
    pub fn new(platform: ConsolePlatform, cfg: &ConsoleStoreConfig) -> Result<Option<Self>> {
        if cfg.client_id.is_empty() {
            return Ok(None);
        }
        let mut c = cfg.clone();
        let p = platform;
        let (url, token_url, scope, verify, consume) = match p {
            ConsolePlatform::Psn => (
                "https://e1-np.entitlement.api.playstation.com",
                "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token",
                "psn:s2s",
                "/v1/users/{user}/entitlements?entitlement_label={product}&service_label={service_label}",
                "/v1/users/{user}/entitlements/{entitlement}?service_label={service_label}",
            ),
            ConsolePlatform::Xbox => (
                "https://collections.mp.microsoft.com",
                "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token",
                "https://onestore.microsoft.com/.default",
                "/v9.0/collections/query",
                "/v9.0/collections/consume",
            ),
            ConsolePlatform::Nintendo => (
                "",
                "https://accounts.nintendo.com/connect/1.0.0/api/token",
                "",
                "/v1/users/{user}/consumable_service_items?item_id={product}",
                "/v1/users/{user}/consumable_service_items/{entitlement}/consume",
            ),
            ConsolePlatform::Epic => (
                "https://api.epicgames.dev",
                "https://api.epicgames.dev/epic/oauth/v1/token",
                "",
                "/epic/ecom/v1/identities/{user}/entitlements?sandboxId={sandbox}&entitlementName={product}",
                "/epic/ecom/v1/identities/{user}/entitlements/{entitlement}/redeem?sandboxId={sandbox}",
            ),
        };
        c.url = or_default(&c.url, url).trim_end_matches('/').to_string();
        c.token_url = or_default(&c.token_url, token_url).replace("{tenant}", &c.tenant_id);
        c.scope = or_default(&c.scope, scope);
        c.verify_path = or_default(&c.verify_path, verify);
        c.consume_path = or_default(&c.consume_path, consume);
        c.service_label = or_default(&c.service_label, "0");
        if c.url.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "iap.{}: url is required",
                p.as_str()
            )));
        }
        if p == ConsolePlatform::Xbox && c.tenant_id.is_empty() && c.token_url.contains("{tenant}")
        {
            return Err(Error::InvalidArgument(
                "iap.xbox: tenant_id is required".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(Error::internal)?;
        Ok(Some(Self {
            platform: p,
            http,
            cfg: c,
            token: Mutex::new(None),
        }))
    }

    pub fn platform(&self) -> ConsolePlatform {
        self.platform
    }

    fn fill(&self, tpl: &str, user: &str, product: &str, entitlement: &str) -> String {
        let enc = crate::path_segment;
        format!(
            "{}{}",
            self.cfg.url,
            tpl.replace("{user}", &enc(user))
                .replace("{product}", &enc(product))
                .replace("{entitlement}", &enc(entitlement))
                .replace("{sandbox}", &enc(&self.cfg.sandbox_id))
                .replace("{service_label}", &enc(&self.cfg.service_label))
        )
    }

    fn tag(&self, what: &str, e: impl std::fmt::Display) -> Error {
        Error::Unavailable(format!("{} {what}: {e}", self.platform.as_str()))
    }

    async fn access_token(&self) -> Result<String> {
        if let Some((t, until)) = self.token.lock().await.clone() {
            if Instant::now() < until {
                return Ok(t);
            }
        }
        let mut form: Vec<(&str, &str)> = vec![("grant_type", "client_credentials")];
        if !self.cfg.scope.is_empty() {
            form.push(("scope", &self.cfg.scope));
        }
        if self.platform == ConsolePlatform::Epic && !self.cfg.deployment_id.is_empty() {
            form.push(("deployment_id", &self.cfg.deployment_id));
        }
        // Xbox (Azure AD) wants the credentials in the form; the others use HTTP basic auth.
        let req = self.http.post(&self.cfg.token_url);
        let req = if self.platform == ConsolePlatform::Xbox {
            form.push(("client_id", &self.cfg.client_id));
            form.push(("client_secret", &self.cfg.client_secret));
            req
        } else {
            req.basic_auth(&self.cfg.client_id, Some(&self.cfg.client_secret))
        };
        let res = req
            .form(&form)
            .send()
            .await
            .map_err(|e| self.tag("token", e))?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        let Some(t) = v["access_token"].as_str() else {
            return Err(Error::Internal(format!(
                "{} token: {status} {v}",
                self.platform.as_str()
            )));
        };
        let ttl = v["expires_in"]
            .as_u64()
            .or_else(|| v["expires_in"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(3600)
            .saturating_sub(60)
            .max(60);
        *self.token.lock().await = Some((t.to_string(), Instant::now() + Duration::from_secs(ttl)));
        Ok(t.to_string())
    }

    async fn call(&self, req: reqwest::RequestBuilder, what: &str) -> Result<Value> {
        let token = self.access_token().await?;
        let res = req
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| self.tag(what, e))?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match status {
            200..=299 => Ok(v),
            400 | 404 => {
                tracing::debug!(
                    platform = self.platform.as_str(),
                    what,
                    status,
                    body = %text.chars().take(200).collect::<String>(),
                    "console store: entitlement not found"
                );
                Err(Error::NotFound(format!(
                    "{} {what}: not found",
                    self.platform.as_str()
                )))
            }
            401 | 403 => {
                self.token.lock().await.take();
                Err(Error::Internal(format!(
                    "{} {what}: rejected server credentials",
                    self.platform.as_str()
                )))
            }
            s => Err(self.tag(what, format!("status {s}"))),
        }
    }

    /// Everything the user owns that matches `product_id` (all entitlements when it is empty).
    pub async fn entitlements(&self, user: &str, product_id: &str) -> Result<Vec<Entitlement>> {
        if user.is_empty() || user.len() > 256 || product_id.len() > 256 {
            return Err(Error::InvalidArgument("user / product_id".into()));
        }
        let url = self.fill(&self.cfg.verify_path, user, product_id, "");
        let v = match self.platform {
            ConsolePlatform::Xbox => {
                let mut body = json!({
                    "beneficiaries": [{ "identityType": "b2b", "identityValue": user }],
                    "validityType": "All",
                    "market": "neutral",
                });
                if !product_id.is_empty() {
                    body["productSkuIds"] = json!([{ "productId": product_id }]);
                }
                self.call(self.http.post(url).json(&body), "collections")
                    .await?
            }
            _ => self.call(self.http.get(url), "entitlements").await?,
        };
        let list = ["entitlements", "items", "consumable_service_items", "data"]
            .iter()
            .find_map(|k| v[*k].as_array().cloned())
            .or_else(|| v.as_array().cloned())
            .unwrap_or_default();
        let mut out = Vec::new();
        for e in list {
            let ent = self.parse(&e);
            if !product_id.is_empty() && ent.product_id != product_id {
                continue;
            }
            out.push(ent);
        }
        Ok(out)
    }

    fn parse(&self, e: &Value) -> Entitlement {
        let s = |keys: &[&str]| -> String {
            keys.iter()
                .find_map(|k| e[*k].as_str().map(str::to_string))
                .unwrap_or_default()
        };
        let n = |keys: &[&str]| -> Option<i64> { keys.iter().find_map(|k| e[*k].as_i64()) };
        let ts = |keys: &[&str]| -> i64 {
            keys.iter()
                .find_map(|k| {
                    e[*k]
                        .as_str()
                        .and_then(|d| {
                            time::OffsetDateTime::parse(
                                d,
                                &time::format_description::well_known::Rfc3339,
                            )
                            .ok()
                        })
                        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
                        .or_else(|| e[*k].as_i64())
                })
                .unwrap_or(0)
        };
        match self.platform {
            ConsolePlatform::Psn => {
                let use_count = n(&["use_count"]).unwrap_or(0);
                let use_limit = n(&["use_limit"]).unwrap_or(0);
                let consumable =
                    e["consumable_flag"].as_bool().unwrap_or(use_limit > 0) || use_limit > 0;
                Entitlement {
                    id: s(&["id", "entitlement_id"]),
                    product_id: s(&["entitlement_label", "label", "id"]),
                    active: e["active_flag"].as_bool().unwrap_or(true),
                    consumable,
                    quantity: if consumable {
                        (use_limit - use_count).max(0)
                    } else {
                        1
                    },
                    acquired_at_ms: ts(&["active_date", "created_date"]),
                    raw: e.clone(),
                }
            }
            ConsolePlatform::Xbox => {
                let kind = s(&["productKind"]);
                Entitlement {
                    id: s(&["id", "itemId", "trackingId"]),
                    product_id: s(&["productId"]),
                    active: s(&["status"]).eq_ignore_ascii_case("active"),
                    consumable: kind.eq_ignore_ascii_case("consumable")
                        || kind.eq_ignore_ascii_case("unmanagedconsumable"),
                    quantity: n(&["quantity"]).unwrap_or(1),
                    acquired_at_ms: ts(&["acquiredDate", "startDate"]),
                    raw: e.clone(),
                }
            }
            ConsolePlatform::Nintendo => {
                let status = s(&["status"]);
                Entitlement {
                    id: s(&["id", "rights_id", "transaction_id"]),
                    product_id: s(&["item_id", "consumable_service_item_id", "ns_uid"]),
                    active: status.is_empty() || status.eq_ignore_ascii_case("purchased"),
                    consumable: true,
                    quantity: n(&["quantity", "count"]).unwrap_or(1),
                    acquired_at_ms: ts(&["purchased_at", "created_at"]),
                    raw: e.clone(),
                }
            }
            ConsolePlatform::Epic => {
                let consumable = e["consumable"].as_bool().unwrap_or(false);
                let uses = n(&["useCount"]).unwrap_or(0);
                Entitlement {
                    id: s(&["id", "entitlementId"]),
                    product_id: s(&["entitlementName", "catalogItemId"]),
                    active: s(&["status"]).eq_ignore_ascii_case("active"),
                    consumable,
                    quantity: if consumable {
                        n(&["originalUseCount"]).unwrap_or(1) - uses
                    } else {
                        1
                    }
                    .max(0),
                    acquired_at_ms: ts(&["grantDate", "created"]),
                    raw: e.clone(),
                }
            }
        }
    }

    /// Mark one use of a consumable; returns the platform's receipt id for this consumption.
    pub async fn consume(&self, user: &str, ent: &Entitlement, quantity: i64) -> Result<String> {
        let url = self.fill(&self.cfg.consume_path, user, &ent.product_id, &ent.id);
        let tracking = uuid::Uuid::now_v7().to_string();
        let v = match self.platform {
            ConsolePlatform::Xbox => {
                self.call(
                    self.http.post(url).json(&json!({
                        "beneficiary": { "identityType": "b2b", "identityValue": user },
                        "productId": ent.product_id,
                        "quantity": quantity,
                        "trackingId": tracking,
                    })),
                    "consume",
                )
                .await?
            }
            ConsolePlatform::Psn => {
                self.call(
                    self.http.put(url).json(&json!({ "use_count": quantity })),
                    "consume",
                )
                .await?
            }
            _ => {
                self.call(self.http.post(url).json(&json!({})), "consume")
                    .await?
            }
        };
        Ok(v["trackingId"]
            .as_str()
            .or(v["transaction_id"].as_str())
            .or(v["id"].as_str())
            .map(str::to_string)
            .unwrap_or(tracking))
    }

    /// Verify that `user` owns `product_id` and turn that into a [`StoreTransaction`]. Consumables
    /// are consumed (one use) unless `consume = false`; durables redeem once per entitlement id.
    pub async fn redeem(&self, user: &str, product_id: &str) -> Result<StoreTransaction> {
        if product_id.is_empty() {
            return Err(Error::InvalidArgument("product_id".into()));
        }
        let ents = self.entitlements(user, product_id).await?;
        let Some(ent) = ents.iter().find(|e| e.active && e.quantity > 0) else {
            return Err(Error::NotFound(format!(
                "{}: no active entitlement for {product_id}",
                self.platform.as_str()
            )));
        };
        let consume = ent.consumable && self.cfg.consume.unwrap_or(true);
        let transaction_id = if consume {
            let receipt = self.consume(user, ent, 1).await?;
            format!("{}:{receipt}", ent.id)
        } else {
            ent.id.clone()
        };
        Ok(StoreTransaction {
            provider: self.platform.as_str().into(),
            transaction_id,
            product_id: product_id.into(),
            environment: if self.cfg.sandbox {
                "Sandbox".into()
            } else {
                "Production".into()
            },
            purchased_at_ms: ent.acquired_at_ms,
            quantity: 1,
            original_transaction_id: Some(ent.id.clone()),
            expires_at_ms: None,
            raw: ent.raw.clone(),
        })
    }
}
