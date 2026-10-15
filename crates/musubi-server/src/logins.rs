//! Third-party sign-in verification: Steam session tickets through the Steamworks Web API and
//! Google / Apple ID tokens against their JWKS. Each verifier returns the stable subject that
//! becomes the identity `subject` (`steam` → steamid64, `google`/`apple` → `sub`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use musubi_core::config::{OidcLoginConfig, SocialConfig};
use musubi_core::{Error, Result};
use serde::Deserialize;

const JWKS_TTL: Duration = Duration::from_secs(3600);

pub struct Social {
    http: reqwest::Client,
    cfg: SocialConfig,
    jwks: Mutex<HashMap<String, (Instant, JwkSet)>>,
}

#[derive(Debug, Deserialize)]
pub struct IdClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<serde_json::Value>,
}

impl Social {
    pub fn new(cfg: SocialConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            cfg,
            jwks: Mutex::new(HashMap::new()),
        }
    }

    /// Steam: `ISteamUserAuth/AuthenticateUserTicket` → steamid64; VAC/publisher-banned users are refused.
    pub async fn steam(&self, ticket: &str) -> Result<String> {
        let c = &self.cfg.steam;
        if c.web_api_key.is_empty() || c.app_id == 0 {
            return Err(Error::InvalidArgument(
                "steam login is not configured".into(),
            ));
        }
        if ticket.is_empty()
            || ticket.len() > 4096
            || !ticket.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Unauthenticated("malformed steam ticket".into()));
        }
        #[derive(Deserialize)]
        struct Resp {
            response: Inner,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Inner {
            params: Option<Params>,
            error: Option<serde_json::Value>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Params {
            result: String,
            steamid: String,
            vacbanned: bool,
            publisherbanned: bool,
        }
        let url = format!(
            "{}/ISteamUserAuth/AuthenticateUserTicket/v1/",
            c.url.trim_end_matches('/')
        );
        let resp: Resp = self
            .http
            .get(url)
            .query(&[
                ("key", c.web_api_key.as_str()),
                ("appid", &c.app_id.to_string()),
                ("ticket", ticket),
            ])
            .send()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("steam api: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {e}")))?;
        let Some(p) = resp.response.params else {
            return Err(Error::Unauthenticated(format!(
                "steam rejected the ticket: {}",
                resp.response.error.unwrap_or_default()
            )));
        };
        if p.result != "OK" || p.steamid.is_empty() {
            return Err(Error::Unauthenticated("steam ticket invalid".into()));
        }
        if p.vacbanned || p.publisherbanned {
            return Err(Error::PermissionDenied("steam account banned".into()));
        }
        Ok(p.steamid)
    }

    pub async fn google(&self, id_token: &str) -> Result<IdClaims> {
        self.verify_oidc(&self.cfg.google, id_token).await
    }

    pub async fn apple(&self, id_token: &str) -> Result<IdClaims> {
        self.verify_oidc(&self.cfg.apple, id_token).await
    }

    async fn verify_oidc(&self, c: &OidcLoginConfig, token: &str) -> Result<IdClaims> {
        if c.jwks_url.is_empty() {
            return Err(Error::InvalidArgument(
                "this login provider is not configured".into(),
            ));
        }
        let header =
            decode_header(token).map_err(|e| Error::Unauthenticated(format!("id token: {e}")))?;
        let kid = header
            .kid
            .ok_or_else(|| Error::Unauthenticated("id token without kid".into()))?;
        let mut set = self.jwks(&c.jwks_url, false).await?;
        if set.find(&kid).is_none() {
            set = self.jwks(&c.jwks_url, true).await?;
        }
        let jwk = set
            .find(&kid)
            .ok_or_else(|| Error::Unauthenticated("unknown signing key".into()))?;
        let key =
            DecodingKey::from_jwk(jwk).map_err(|e| Error::Unauthenticated(format!("jwk: {e}")))?;
        let mut v = Validation::new(header.alg);
        v.set_issuer(&c.issuers);
        if c.client_ids.is_empty() {
            v.validate_aud = false;
        } else {
            v.set_audience(&c.client_ids);
        }
        v.set_required_spec_claims(&["exp", "iss", "sub"]);
        let data = decode::<IdClaims>(token, &key, &v)
            .map_err(|e| Error::Unauthenticated(format!("id token: {e}")))?;
        Ok(data.claims)
    }

    async fn jwks(&self, url: &str, refresh: bool) -> Result<JwkSet> {
        if !refresh {
            let cache = self.jwks.lock().unwrap();
            if let Some((at, set)) = cache.get(url) {
                if at.elapsed() < JWKS_TTL {
                    return Ok(set.clone());
                }
            }
        }
        let set: JwkSet = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("jwks: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("jwks: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("jwks: {e}")))?;
        self.jwks
            .lock()
            .unwrap()
            .insert(url.to_string(), (Instant::now(), set.clone()));
        Ok(set)
    }
}
