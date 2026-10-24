//! Third-party sign-in verification: Steam session tickets through the Steamworks Web API and
//! Google / Apple ID tokens against their JWKS, plus any configured OpenID Connect issuer
//! (`[social.oidc.<name>]`, JWKS found through discovery). Each verifier returns the stable
//! subject that becomes the identity `subject` (`steam` → steamid64, OIDC → `sub`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use musubi_core::config::{oidc_preset, OidcLoginConfig, SocialConfig};
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};

const JWKS_TTL: Duration = Duration::from_secs(3600);
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(60);
/// Signature algorithms accepted on provider id tokens (RFC 7518 asymmetric families only).
const ID_TOKEN_ALGS: &[jsonwebtoken::Algorithm] = &[
    jsonwebtoken::Algorithm::RS256,
    jsonwebtoken::Algorithm::RS384,
    jsonwebtoken::Algorithm::RS512,
    jsonwebtoken::Algorithm::PS256,
    jsonwebtoken::Algorithm::PS384,
    jsonwebtoken::Algorithm::PS512,
    jsonwebtoken::Algorithm::ES256,
    jsonwebtoken::Algorithm::ES384,
    jsonwebtoken::Algorithm::EdDSA,
];

pub struct Social {
    http: reqwest::Client,
    cfg: SocialConfig,
    jwks: Mutex<HashMap<String, (Instant, JwkSet)>>,
    /// issuer → discovered `jwks_uri`
    discovery: Mutex<HashMap<String, (Instant, String)>>,
    /// Game Center public-key certificates by URL.
    certs: Mutex<HashMap<String, (Instant, Vec<u8>)>>,
}

/// Identity provider name for a configured OIDC issuer.
pub fn oidc_provider(name: &str) -> String {
    format!("oidc:{name}")
}

pub fn valid_oidc_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[derive(Debug, Deserialize)]
pub struct IdClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<serde_json::Value>,
    /// PSN puts the player's online id in the id token.
    #[serde(default)]
    pub online_id: Option<String>,
    /// Every other claim (groups, roles, name…).
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// What the client gets from `GKLocalPlayer.fetchItems(forIdentityVerificationSignature:)`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameCenterSig {
    pub player_id: String,
    pub bundle_id: String,
    /// Milliseconds since the epoch, as Apple hands it out.
    pub timestamp: u64,
    /// base64
    pub salt: String,
    /// base64
    pub signature: String,
    pub public_key_url: String,
}

/// Answer of `ISteamUser/CheckAppOwnership` for the configured app.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SteamOwnership {
    pub owns_app: bool,
    pub permanent: bool,
    pub timed_trial: bool,
    pub site_license: bool,
    /// `ownersteamid` is another account: the player borrows the game through Family Sharing.
    pub family_shared: bool,
    pub owner_steamid: String,
}

#[derive(Debug, Clone)]
pub struct XboxProfile {
    pub xuid: String,
    pub gamertag: Option<String>,
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
            discovery: Mutex::new(HashMap::new()),
            certs: Mutex::new(HashMap::new()),
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
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
            .map_err(|e| Error::Internal(format!("steam api: {}", e.without_url())))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("steam api: {}", e.without_url())))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {}", e.without_url())))?;
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

    /// Steam: `ISteamUser/CheckAppOwnership` for `app_id` — ownership, Family Sharing lender
    /// (`ownersteamid` differs from the player), permanence and timed trials.
    /// Encrypted app ticket (`ISteamUser::RequestEncryptedAppTicket`) → verified ticket, offline
    /// with `[social.steam].app_ticket_key`; must be for `app_id` (or one of its DLC) and younger
    /// than `app_ticket_max_age_secs`.
    pub fn steam_app_ticket(&self, ticket: &str) -> Result<crate::steam_ticket::AppTicket> {
        let c = &self.cfg.steam;
        if c.app_ticket_key.is_empty() {
            return Err(Error::InvalidArgument(
                "steam app tickets are not configured ([social.steam].app_ticket_key)".into(),
            ));
        }
        let key: [u8; 32] = hex::decode(c.app_ticket_key.trim())
            .ok()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| {
                Error::Internal("[social.steam].app_ticket_key must be 64 hex chars".into())
            })?;
        let raw = crate::steam_ticket::decode_text(ticket)?;
        let t = crate::steam_ticket::decrypt(&raw, &key)?;
        if c.app_id != 0 && t.app_id != c.app_id && !t.dlc.iter().any(|d| d.app_id == c.app_id) {
            return Err(Error::Unauthenticated(format!(
                "steam app ticket is for app {}, not {}",
                t.app_id, c.app_id
            )));
        }
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if c.app_ticket_max_age_secs > 0 && now - t.issued_at > c.app_ticket_max_age_secs as i64 {
            return Err(Error::Unauthenticated(
                "steam app ticket is too old; request a fresh one".into(),
            ));
        }
        Ok(t)
    }

    /// Persona name + avatar from `ISteamUser/GetPlayerSummaries` (best effort; `None` without a
    /// Web API key or when Steam does not answer).
    pub async fn steam_persona(&self, steam_id: &str) -> Option<(String, Option<String>)> {
        let c = &self.cfg.steam;
        if c.web_api_key.is_empty() {
            return None;
        }
        let url = format!(
            "{}/ISteamUser/GetPlayerSummaries/v2/",
            c.url.trim_end_matches('/')
        );
        let v: serde_json::Value = self
            .http
            .get(url)
            .query(&[("key", c.web_api_key.as_str()), ("steamids", steam_id)])
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let p = v["response"]["players"].get(0)?;
        let name = p["personaname"].as_str()?.to_string();
        Some((name, p["avatarfull"].as_str().map(str::to_string)))
    }

    pub async fn steam_ownership(&self, steam_id: &str) -> Result<SteamOwnership> {
        let c = &self.cfg.steam;
        if c.web_api_key.is_empty() || c.app_id == 0 {
            return Err(Error::InvalidArgument(
                "steam login is not configured".into(),
            ));
        }
        let url = format!(
            "{}/ISteamUser/CheckAppOwnership/v4/",
            c.url.trim_end_matches('/')
        );
        let v: serde_json::Value = self
            .http
            .get(url)
            .query(&[
                ("key", c.web_api_key.as_str()),
                ("steamid", steam_id),
                ("appid", &c.app_id.to_string()),
            ])
            .send()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {}", e.without_url())))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("steam api: {}", e.without_url())))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {}", e.without_url())))?;
        let a = &v["appownership"];
        if a.is_null() {
            return Err(Error::Internal(format!("steam api: unexpected answer {v}")));
        }
        let owner = a["ownersteamid"]
            .as_str()
            .map(str::to_string)
            .or_else(|| a["ownersteamid"].as_u64().map(|n| n.to_string()))
            .unwrap_or_default();
        Ok(SteamOwnership {
            owns_app: a["ownsapp"].as_bool().unwrap_or(false),
            permanent: a["permanent"].as_bool().unwrap_or(false),
            timed_trial: a["timedtrial"].as_bool().unwrap_or(false),
            site_license: a["sitelicense"].as_bool().unwrap_or(false),
            family_shared: a["ownsapp"].as_bool().unwrap_or(false)
                && !owner.is_empty()
                && owner != steam_id,
            owner_steamid: owner,
        })
    }

    /// Xbox Live: the XSTS token the client got for the `http://xboxlive.com` relying party is
    /// presented to the profile service as `XBL3.0 x=<uhs>;<token>`; a 401 there means the
    /// token is forged, expired or for another relying party. Returns the XUID and gamertag.
    pub async fn xbox(&self, user_hash: &str, token: &str) -> Result<XboxProfile> {
        let c = &self.cfg.xbox;
        if c.url.is_empty() {
            return Err(Error::InvalidArgument(
                "xbox login is not configured".into(),
            ));
        }
        let clean = |s: &str| {
            !s.is_empty()
                && s.len() <= 8192
                && s.bytes()
                    .all(|b| b.is_ascii_graphic() && b != b';' && b != b',')
        };
        if !clean(user_hash) || !clean(token) {
            return Err(Error::Unauthenticated("malformed xbox token".into()));
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            #[serde(rename = "profileUsers")]
            profile_users: Vec<User>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct User {
            id: String,
            settings: Vec<Setting>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Setting {
            id: String,
            value: String,
        }
        let url = format!(
            "{}/users/me/profile/settings?settings=Gamertag",
            c.url.trim_end_matches('/')
        );
        let res = self
            .http
            .get(url)
            .header("Authorization", format!("XBL3.0 x={user_hash};{token}"))
            .header("x-xbl-contract-version", c.contract_version.to_string())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| Error::Internal(format!("xbox live: {e}")))?;
        match res.status().as_u16() {
            200 => {}
            401 | 403 => {
                return Err(Error::Unauthenticated(
                    "xbox live rejected the token".into(),
                ))
            }
            s => return Err(Error::Internal(format!("xbox live: status {s}"))),
        }
        let resp: Resp = res
            .json()
            .await
            .map_err(|e| Error::Internal(format!("xbox live: {e}")))?;
        let Some(u) = resp.profile_users.into_iter().next() else {
            return Err(Error::Unauthenticated("xbox live: no profile".into()));
        };
        if u.id.is_empty() || !u.id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Unauthenticated("xbox live: bad xuid".into()));
        }
        let gamertag = u
            .settings
            .into_iter()
            .find(|s| s.id == "Gamertag")
            .map(|s| s.value)
            .filter(|v| !v.is_empty());
        Ok(XboxProfile {
            xuid: u.id,
            gamertag,
        })
    }

    pub async fn psn(&self, id_token: &str) -> Result<IdClaims> {
        self.verify_oidc(&self.cfg.psn, id_token).await
    }

    pub async fn nintendo(&self, id_token: &str) -> Result<IdClaims> {
        self.verify_oidc(&self.cfg.nintendo, id_token).await
    }

    pub async fn google(&self, id_token: &str) -> Result<IdClaims> {
        self.verify_oidc(&self.cfg.google, id_token).await
    }

    pub async fn apple(&self, id_token: &str) -> Result<IdClaims> {
        self.verify_oidc(&self.cfg.apple, id_token).await
    }

    /// A configured generic issuer (`[social.oidc.<name>]`).
    pub async fn oidc(&self, name: &str, id_token: &str) -> Result<IdClaims> {
        let c = self
            .cfg
            .oidc
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("unknown login provider {name}")))?;
        if c.jwks_url.is_empty() && c.issuer.is_empty() {
            if let Some(mut preset) = oidc_preset(name) {
                preset.client_ids = c.client_ids.clone();
                if !c.issuers.is_empty() {
                    preset.issuers = c.issuers.clone();
                }
                return self.verify_oidc(&preset, id_token).await;
            }
        }
        self.verify_oidc(c, id_token).await
    }

    /// Facebook: a Limited Login id token (JWT) is verified against Facebook's OIDC keys with
    /// the app id as audience; a classic access token is inspected with `debug_token` under the
    /// app's credentials and must belong to this app and be valid. Returns the app-scoped user id.
    pub async fn facebook(&self, token: &str) -> Result<String> {
        let c = &self.cfg.facebook;
        if c.app_id.is_empty() {
            return Err(Error::InvalidArgument(
                "facebook login is not configured".into(),
            ));
        }
        if token.is_empty() || token.len() > 4096 {
            return Err(Error::Unauthenticated("malformed facebook token".into()));
        }
        if token.matches('.').count() == 2 && !token.contains('|') {
            let mut oidc = c.oidc.clone();
            if oidc.client_ids.is_empty() {
                oidc.client_ids = vec![c.app_id.clone()];
            }
            return Ok(self.verify_oidc(&oidc, token).await?.sub);
        }
        if c.app_secret.is_empty() {
            return Err(Error::InvalidArgument(
                "facebook access tokens need social.facebook.app_secret".into(),
            ));
        }
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            data: Data,
            #[serde(default)]
            error: Option<serde_json::Value>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Data {
            app_id: String,
            is_valid: bool,
            user_id: String,
            error: Option<serde_json::Value>,
        }
        let url = format!("{}/debug_token", c.url.trim_end_matches('/'));
        let app_token = format!("{}|{}", c.app_id, c.app_secret);
        let resp: Resp = self
            .http
            .get(url)
            .query(&[("input_token", token), ("access_token", app_token.as_str())])
            .send()
            .await
            .map_err(|e| Error::Internal(format!("facebook api: {}", e.without_url())))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("facebook api: {}", e.without_url())))?;
        if let Some(e) = resp.error.or(resp.data.error) {
            return Err(Error::Unauthenticated(format!(
                "facebook rejected the token: {e}"
            )));
        }
        if !resp.data.is_valid || resp.data.user_id.is_empty() {
            return Err(Error::Unauthenticated("facebook token invalid".into()));
        }
        if resp.data.app_id != c.app_id {
            return Err(Error::Unauthenticated(
                "facebook token is for another app".into(),
            ));
        }
        Ok(resp.data.user_id)
    }

    /// Game Center: the signature from `fetchItems(forIdentityVerificationSignature:)` is
    /// RSA-SHA256 over `player_id ‖ bundle_id ‖ timestamp(be u64) ‖ salt`, made with the key of
    /// the certificate at `public_key_url` (which must live on Apple's hosts). Returns `player_id`.
    pub async fn gamecenter(&self, r: &GameCenterSig) -> Result<String> {
        use base64::Engine;
        let c = &self.cfg.gamecenter;
        if c.bundle_ids.is_empty() {
            return Err(Error::InvalidArgument(
                "game center login is not configured".into(),
            ));
        }
        if r.player_id.is_empty() || r.player_id.len() > 128 {
            return Err(Error::Unauthenticated(
                "malformed game center player id".into(),
            ));
        }
        if !c.bundle_ids.contains(&r.bundle_id) {
            return Err(Error::Unauthenticated("unexpected bundle id".into()));
        }
        if !c
            .public_key_url_prefixes
            .iter()
            .any(|p| r.public_key_url.starts_with(p))
        {
            return Err(Error::Unauthenticated(
                "public_key_url is not an Apple certificate".into(),
            ));
        }
        if c.max_skew_secs > 0 {
            let now = time::OffsetDateTime::now_utc().unix_timestamp() as i128 * 1000;
            if (now - r.timestamp as i128).unsigned_abs() > c.max_skew_secs as u128 * 1000 {
                return Err(Error::Unauthenticated(
                    "game center signature expired".into(),
                ));
            }
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        let salt = b64
            .decode(&r.salt)
            .map_err(|_| Error::Unauthenticated("salt is not base64".into()))?;
        let sig = b64
            .decode(&r.signature)
            .map_err(|_| Error::Unauthenticated("signature is not base64".into()))?;
        let der = self.certificate(&r.public_key_url).await?;
        let (_, cert) = x509_parser::parse_x509_certificate(&der)
            .map_err(|e| Error::Unauthenticated(format!("certificate: {e}")))?;
        if !cert.validity().is_valid() {
            return Err(Error::Unauthenticated(
                "certificate is expired or not yet valid".into(),
            ));
        }
        let spki = cert.public_key().subject_public_key.data.to_vec();
        let mut payload =
            Vec::with_capacity(r.player_id.len() + r.bundle_id.len() + 8 + salt.len());
        payload.extend_from_slice(r.player_id.as_bytes());
        payload.extend_from_slice(r.bundle_id.as_bytes());
        payload.extend_from_slice(&r.timestamp.to_be_bytes());
        payload.extend_from_slice(&salt);
        ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, spki)
            .verify(&payload, &sig)
            .map_err(|_| Error::Unauthenticated("game center signature does not verify".into()))?;
        Ok(r.player_id.clone())
    }

    async fn certificate(&self, url: &str) -> Result<Vec<u8>> {
        if let Some((at, der)) = self.certs.lock().unwrap().get(url) {
            if at.elapsed() < JWKS_TTL {
                return Ok(der.clone());
            }
        }
        let bytes = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("game center certificate: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Unauthenticated(format!("game center certificate: {e}")))?
            .bytes()
            .await
            .map_err(|e| Error::Internal(format!("game center certificate: {e}")))?;
        if bytes.len() > 64 * 1024 {
            return Err(Error::Unauthenticated("certificate too large".into()));
        }
        let der = bytes.to_vec();
        self.certs
            .lock()
            .unwrap()
            .insert(url.to_string(), (Instant::now(), der.clone()));
        Ok(der)
    }

    /// Verify an ID token against an arbitrary issuer config (operator SSO).
    pub async fn verify_id_token(&self, c: &OidcLoginConfig, token: &str) -> Result<IdClaims> {
        self.verify_oidc(c, token).await
    }

    /// The provider's `/.well-known/openid-configuration` document.
    pub async fn discover(&self, issuer: &str) -> Result<serde_json::Value> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        self.http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("oidc discovery: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("oidc discovery: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("oidc discovery: {e}")))
    }

    async fn verify_oidc(&self, c: &OidcLoginConfig, token: &str) -> Result<IdClaims> {
        if c.jwks_url.is_empty() && c.issuer.is_empty() {
            return Err(Error::InvalidArgument(
                "this login provider is not configured".into(),
            ));
        }
        let jwks_url = if c.jwks_url.is_empty() {
            self.discover_jwks(&c.issuer).await?
        } else {
            c.jwks_url.clone()
        };
        let issuers: Vec<String> = if c.issuers.is_empty() && !c.issuer.is_empty() {
            vec![c.issuer.clone()]
        } else {
            c.issuers.clone()
        };
        let header =
            decode_header(token).map_err(|e| Error::Unauthenticated(format!("id token: {e}")))?;
        let kid = header
            .kid
            .ok_or_else(|| Error::Unauthenticated("id token without kid".into()))?;
        let mut set = self.jwks(&jwks_url, false).await?;
        if set.find(&kid).is_none() {
            set = self.jwks(&jwks_url, true).await?;
        }
        let jwk = set
            .find(&kid)
            .ok_or_else(|| Error::Unauthenticated("unknown signing key".into()))?;
        let key =
            DecodingKey::from_jwk(jwk).map_err(|e| Error::Unauthenticated(format!("jwk: {e}")))?;
        // only asymmetric signatures: an HS* token would be verified with the public JWK bytes
        if !ID_TOKEN_ALGS.contains(&header.alg) {
            return Err(Error::Unauthenticated(format!(
                "id token: algorithm {:?} is not allowed",
                header.alg
            )));
        }
        let mut v = Validation::new(header.alg);
        v.set_issuer(&issuers);
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

    /// `<issuer>/.well-known/openid-configuration` → `jwks_uri`, cached for the JWKS TTL.
    async fn discover_jwks(&self, issuer: &str) -> Result<String> {
        if let Some((at, uri)) = self.discovery.lock().unwrap().get(issuer) {
            if at.elapsed() < JWKS_TTL {
                return Ok(uri.clone());
            }
        }
        #[derive(Deserialize)]
        struct Doc {
            jwks_uri: String,
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let doc: Doc = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Internal(format!("oidc discovery: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("oidc discovery: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("oidc discovery: {e}")))?;
        self.discovery
            .lock()
            .unwrap()
            .insert(issuer.to_string(), (Instant::now(), doc.jwks_uri.clone()));
        Ok(doc.jwks_uri)
    }

    async fn jwks(&self, url: &str, refresh: bool) -> Result<JwkSet> {
        {
            // an unknown `kid` forces a refresh, but at most once per JWKS_MIN_REFRESH per URL so
            // forged tokens cannot make the node hammer the provider
            let max_age = if refresh { JWKS_MIN_REFRESH } else { JWKS_TTL };
            let cache = self.jwks.lock().unwrap();
            if let Some((at, set)) = cache.get(url) {
                if at.elapsed() < max_age {
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
