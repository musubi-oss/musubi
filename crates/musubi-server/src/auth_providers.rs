//! Generic OAuth2 / OpenID Connect provider catalog: any IdP (Discord, Twitch, Epic, Keycloak,
//! Auth0, GitHub, …) configured at runtime, authorization-code flow with PKCE run by the server,
//! claims mapped onto `oidc:<name>` identities and the player's profile. Supplements the static
//! `[social.oidc]` id-token login with the same identity namespace.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use musubi_core::config::OidcLoginConfig;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::logins::Social;

pub const FLOW_TTL_SECS: i64 = 600;
const DISCOVERY_TTL: Duration = Duration::from_secs(600);

/// Which claim (dotted JSON path) feeds which identity / profile field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClaimsMap {
    pub subject: String,
    pub email: String,
    pub email_verified: String,
    pub display_name: String,
    pub avatar_url: String,
    pub username: String,
}

impl Default for ClaimsMap {
    fn default() -> Self {
        Self {
            subject: "sub".into(),
            email: "email".into(),
            email_verified: "email_verified".into(),
            display_name: "name".into(),
            avatar_url: "picture".into(),
            username: "preferred_username".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderInput {
    /// `oidc` (id_token verified against JWKS, discovery from `issuer`), `oauth2` (plain access
    /// token + `userinfo_url`) or `openid2` (OpenID 2.0 relying party — Steam's browser login:
    /// `authorize_url` defaults to `https://steamcommunity.com/openid/login`, `claims.subject`
    /// `steam_id` maps the identity onto the `steam` provider, `claimed_id` keeps it generic).
    pub kind: String,
    pub display_name: String,
    pub enabled: bool,
    pub issuer: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub jwks_url: String,
    pub client_id: String,
    /// Absent = keep the stored secret; empty string = public client.
    pub client_secret: Option<String>,
    /// `body` (client_secret_post) or `basic` (client_secret_basic).
    pub auth_style: String,
    pub scopes: Vec<String>,
    pub pkce: bool,
    pub claims: ClaimsMap,
    pub allow_signup: bool,
    /// Callback URLs registered at the IdP; the first is the default, native apps may pick another.
    pub redirect_urls: Vec<String>,
    /// Where the browser may be sent after the callback (prefix match); empty = no redirects.
    pub return_urls: Vec<String>,
}

impl Default for ProviderInput {
    fn default() -> Self {
        Self {
            kind: "oidc".into(),
            display_name: String::new(),
            enabled: true,
            issuer: String::new(),
            authorize_url: String::new(),
            token_url: String::new(),
            userinfo_url: String::new(),
            jwks_url: String::new(),
            client_id: String::new(),
            client_secret: None,
            auth_style: "body".into(),
            scopes: Vec::new(),
            pkce: true,
            claims: ClaimsMap::default(),
            allow_signup: true,
            redirect_urls: Vec::new(),
            return_urls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub name: String,
    pub kind: String,
    pub display_name: String,
    pub enabled: bool,
    pub issuer: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub jwks_url: String,
    pub client_id: String,
    #[serde(skip)]
    pub client_secret: String,
    pub has_client_secret: bool,
    pub auth_style: String,
    pub scopes: Vec<String>,
    pub pkce: bool,
    pub claims: ClaimsMap,
    pub allow_signup: bool,
    pub redirect_urls: Vec<String>,
    pub return_urls: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Provider {
    /// What a client needs to render the login button.
    pub fn public(&self) -> Value {
        json!({
            "name": self.name,
            "display_name": if self.display_name.is_empty() { &self.name } else { &self.display_name },
            "kind": self.kind,
            "scopes": self.scopes,
            "pkce": self.pkce,
            "redirect_urls": self.redirect_urls,
            "allow_signup": self.allow_signup,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ProviderRow {
    name: String,
    kind: String,
    display_name: String,
    enabled: bool,
    issuer: String,
    authorize_url: String,
    token_url: String,
    userinfo_url: String,
    jwks_url: String,
    client_id: String,
    client_secret: String,
    auth_style: String,
    scopes: Vec<String>,
    pkce: bool,
    claims: Value,
    allow_signup: bool,
    redirect_urls: Vec<String>,
    return_urls: Vec<String>,
    updated_at: OffsetDateTime,
}

const PROVIDER_COLS: &str = "name, kind, display_name, enabled, issuer, authorize_url, token_url, userinfo_url, jwks_url, client_id, client_secret, auth_style, scopes, pkce, claims, allow_signup, redirect_urls, return_urls, updated_at";

fn row_to_provider(r: ProviderRow) -> Provider {
    let client_secret = musubi_core::secrets::open(&r.client_secret).unwrap_or_else(|e| {
        tracing::error!(provider = %r.name, error = %e, "auth provider client secret cannot be opened");
        String::new()
    });
    Provider {
        name: r.name,
        kind: r.kind,
        display_name: r.display_name,
        enabled: r.enabled,
        issuer: r.issuer,
        authorize_url: r.authorize_url,
        token_url: r.token_url,
        userinfo_url: r.userinfo_url,
        jwks_url: r.jwks_url,
        client_id: r.client_id,
        has_client_secret: !r.client_secret.is_empty(),
        client_secret,
        auth_style: r.auth_style,
        scopes: r.scopes,
        pkce: r.pkce,
        claims: serde_json::from_value(r.claims).unwrap_or_default(),
        allow_signup: r.allow_signup,
        redirect_urls: r.redirect_urls,
        return_urls: r.return_urls,
        updated_at: r.updated_at,
    }
}

/// Resolved endpoints (config or discovery).
#[derive(Debug, Clone, Serialize)]
pub struct Endpoints {
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub jwks_url: String,
}

/// Options a client passes when starting a flow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StartOptions {
    /// Browser destination after the callback; must match `return_urls`.
    pub return_url: String,
    /// One of the provider's `redirect_urls` (default: the first).
    pub callback_url: String,
    pub create: Option<bool>,
    pub username: Option<String>,
    pub vars: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Started {
    pub url: String,
    pub state: String,
    pub nonce: String,
    pub expires_in: i64,
}

#[derive(Debug, Clone)]
pub struct Flow {
    pub state: String,
    pub provider: String,
    pub verifier: String,
    pub nonce: String,
    pub callback_url: String,
    pub return_url: String,
    pub account_id: Option<AccountId>,
    pub opts: StartOptions,
}

/// Who the IdP says the user is, after claims mapping.
#[derive(Debug, Clone, Serialize)]
pub struct External {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub username: Option<String>,
    pub claims: Value,
}

pub struct AuthProviderService {
    db: PgPool,
    discovery: Mutex<HashMap<String, (Instant, Value)>>,
}

pub const STEAM_OPENID_ENDPOINT: &str = "https://steamcommunity.com/openid/login";
const OPENID2_NS: &str = "http://specs.openid.net/auth/2.0";
const OPENID2_IDENTIFIER_SELECT: &str = "http://specs.openid.net/auth/2.0/identifier_select";

/// `scheme://host[:port]` of an absolute URL (OpenID realm / claimed_id origin checks).
fn origin(url: &str) -> Result<String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| Error::InvalidArgument(format!("`{url}` is not an absolute URL")))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() {
        return Err(Error::InvalidArgument(format!("`{url}` has no host")));
    }
    Ok(format!("{scheme}://{host}"))
}

pub fn valid_name(name: &str) -> bool {
    crate::logins::valid_oidc_name(name)
}

fn is_http_url(u: &str) -> bool {
    u.starts_with("https://") || u.starts_with("http://")
}

pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_token() -> String {
    let a = Uuid::now_v7().as_u128();
    let b: u128 = rand::random();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(&a.to_be_bytes());
    bytes.extend_from_slice(&b.to_be_bytes());
    b64url(&bytes)
}

pub fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn enc(s: &str) -> String {
    url_escape(s)
}

fn url_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `a.b.c` lookup on a JSON object; numbers/bools are stringified.
pub fn claim_str(claims: &Value, path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let mut cur = claims;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    match cur {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn claim_bool(claims: &Value, path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let mut cur = claims;
    for seg in path.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => return false,
        }
    }
    match cur {
        Value::Bool(b) => *b,
        Value::String(s) => s == "true" || s == "1",
        Value::Number(n) => n.as_i64() == Some(1),
        _ => false,
    }
}

pub fn map_claims(map: &ClaimsMap, claims: &Value) -> Result<External> {
    let subject = claim_str(claims, &map.subject).ok_or_else(|| {
        Error::Unauthenticated(format!(
            "identity provider returned no `{}` claim",
            map.subject
        ))
    })?;
    if subject.len() > 256 {
        return Err(Error::Unauthenticated("subject claim too long".into()));
    }
    let email = claim_str(claims, &map.email)
        .map(|e| e.trim().to_lowercase())
        .filter(|e| e.contains('@') && e.len() <= 254);
    Ok(External {
        subject,
        email_verified: email.is_some() && claim_bool(claims, &map.email_verified),
        email,
        display_name: claim_str(claims, &map.display_name).map(|s| s.chars().take(64).collect()),
        avatar_url: claim_str(claims, &map.avatar_url).filter(|u| is_http_url(u) && u.len() <= 512),
        username: claim_str(claims, &map.username).map(|s| s.chars().take(32).collect()),
        claims: claims.clone(),
    })
}

fn validate(name: &str, p: &ProviderInput) -> Result<()> {
    if !valid_name(name) {
        return Err(Error::InvalidArgument(
            "provider name: 1..=32 of [a-z0-9_-]".into(),
        ));
    }
    if p.kind != "oidc" && p.kind != "oauth2" && p.kind != "openid2" {
        return Err(Error::InvalidArgument(
            "kind must be oidc, oauth2 or openid2".into(),
        ));
    }
    if p.client_id.trim().is_empty() || p.client_id.len() > 512 {
        return Err(Error::InvalidArgument("client_id is required".into()));
    }
    if p.auth_style != "body" && p.auth_style != "basic" {
        return Err(Error::InvalidArgument(
            "auth_style must be body or basic".into(),
        ));
    }
    for (k, u) in [
        ("issuer", &p.issuer),
        ("authorize_url", &p.authorize_url),
        ("token_url", &p.token_url),
        ("userinfo_url", &p.userinfo_url),
        ("jwks_url", &p.jwks_url),
    ] {
        if !u.is_empty() && (!is_http_url(u) || u.len() > 2048) {
            return Err(Error::InvalidArgument(format!(
                "{k} must be an http(s) URL"
            )));
        }
    }
    if p.kind == "oidc" {
        if p.issuer.is_empty()
            && (p.authorize_url.is_empty() || p.token_url.is_empty() || p.jwks_url.is_empty())
        {
            return Err(Error::InvalidArgument(
                "oidc: set issuer (discovery) or authorize_url + token_url + jwks_url".into(),
            ));
        }
    } else if p.kind == "openid2" {
        if p.claims.subject != "steam_id" && p.claims.subject != "claimed_id" {
            return Err(Error::InvalidArgument(
                "openid2: claims.subject must be steam_id or claimed_id".into(),
            ));
        }
        if p.authorize_url.is_empty() {
            return Err(Error::InvalidArgument(
                "openid2: authorize_url (the OP endpoint) is required".into(),
            ));
        }
    } else if p.authorize_url.is_empty() || p.token_url.is_empty() || p.userinfo_url.is_empty() {
        return Err(Error::InvalidArgument(
            "oauth2: authorize_url, token_url and userinfo_url are required".into(),
        ));
    }
    if p.redirect_urls.is_empty() {
        return Err(Error::InvalidArgument(
            "redirect_urls: at least one callback URL registered at the provider".into(),
        ));
    }
    if p.redirect_urls.len() > 16 || p.return_urls.len() > 32 || p.scopes.len() > 32 {
        return Err(Error::InvalidArgument("too many urls/scopes".into()));
    }
    for u in &p.redirect_urls {
        if u.is_empty() || u.len() > 2048 || !u.contains("://") {
            return Err(Error::InvalidArgument(format!(
                "redirect url `{u}` must be absolute"
            )));
        }
    }
    for u in &p.return_urls {
        if u.is_empty() || u.len() > 2048 || !u.contains("://") {
            return Err(Error::InvalidArgument(format!(
                "return url `{u}` must be absolute"
            )));
        }
    }
    if p.claims.subject.is_empty() {
        return Err(Error::InvalidArgument("claims.subject is required".into()));
    }
    Ok(())
}

impl AuthProviderService {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            discovery: Mutex::new(HashMap::new()),
        }
    }

    pub async fn list(&self) -> Result<Vec<Provider>> {
        let rows: Vec<ProviderRow> = sqlx::query_as(&format!(
            "select {PROVIDER_COLS} from musubi.auth_providers order by name"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_provider).collect())
    }

    pub async fn get(&self, name: &str) -> Result<Provider> {
        let row: Option<ProviderRow> = sqlx::query_as(&format!(
            "select {PROVIDER_COLS} from musubi.auth_providers where name = $1"
        ))
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(row_to_provider)
            .ok_or_else(|| Error::NotFound(format!("login provider {name}")))
    }

    /// An enabled provider, or 404 (disabled ones look absent to players).
    pub async fn enabled(&self, name: &str) -> Result<Provider> {
        if !valid_name(name) {
            return Err(Error::InvalidArgument("bad provider name".into()));
        }
        let p = self.get(name).await?;
        if !p.enabled {
            return Err(Error::NotFound(format!("login provider {name}")));
        }
        Ok(p)
    }

    pub async fn put(&self, name: &str, mut p: ProviderInput) -> Result<Provider> {
        if p.kind == "openid2" {
            if p.authorize_url.is_empty() {
                p.authorize_url = STEAM_OPENID_ENDPOINT.into();
            }
            if p.claims.subject == "sub" {
                p.claims.subject = "steam_id".into();
            }
            if p.client_id.is_empty() {
                p.client_id = "openid2".into();
            }
            p.pkce = false;
        }
        validate(name, &p)?;
        let secret = match &p.client_secret {
            Some(s) => s.clone(),
            None => self
                .get(name)
                .await
                .map(|e| e.client_secret)
                .unwrap_or_default(),
        };
        sqlx::query(
            "insert into musubi.auth_providers (name, kind, display_name, enabled, issuer, authorize_url, token_url,
                userinfo_url, jwks_url, client_id, client_secret, auth_style, scopes, pkce, claims, allow_signup,
                redirect_urls, return_urls)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
             on conflict (name) do update set kind = excluded.kind, display_name = excluded.display_name,
                enabled = excluded.enabled, issuer = excluded.issuer, authorize_url = excluded.authorize_url,
                token_url = excluded.token_url, userinfo_url = excluded.userinfo_url, jwks_url = excluded.jwks_url,
                client_id = excluded.client_id, client_secret = excluded.client_secret, auth_style = excluded.auth_style,
                scopes = excluded.scopes, pkce = excluded.pkce, claims = excluded.claims,
                allow_signup = excluded.allow_signup, redirect_urls = excluded.redirect_urls,
                return_urls = excluded.return_urls, updated_at = now()",
        )
        .bind(name)
        .bind(&p.kind)
        .bind(p.display_name.chars().take(64).collect::<String>())
        .bind(p.enabled)
        .bind(p.issuer.trim_end_matches('/'))
        .bind(&p.authorize_url)
        .bind(&p.token_url)
        .bind(&p.userinfo_url)
        .bind(&p.jwks_url)
        .bind(p.client_id.trim())
        .bind(musubi_core::secrets::seal(&secret))
        .bind(&p.auth_style)
        .bind(&p.scopes)
        .bind(p.pkce)
        .bind(serde_json::to_value(&p.claims).unwrap_or_default())
        .bind(p.allow_signup)
        .bind(&p.redirect_urls)
        .bind(&p.return_urls)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        self.discovery.lock().unwrap().remove(&p.issuer);
        self.get(name).await
    }

    pub async fn delete(&self, name: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.auth_providers where name = $1")
            .bind(name)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }

    async fn discover(&self, social: &Social, issuer: &str) -> Result<Value> {
        if let Some((at, doc)) = self.discovery.lock().unwrap().get(issuer) {
            if at.elapsed() < DISCOVERY_TTL {
                return Ok(doc.clone());
            }
        }
        let doc = social.discover(issuer).await?;
        self.discovery
            .lock()
            .unwrap()
            .insert(issuer.to_string(), (Instant::now(), doc.clone()));
        Ok(doc)
    }

    /// Explicit URLs win; the rest comes from `<issuer>/.well-known/openid-configuration`.
    pub async fn endpoints(&self, social: &Social, p: &Provider) -> Result<Endpoints> {
        let mut e = Endpoints {
            authorize_url: p.authorize_url.clone(),
            token_url: p.token_url.clone(),
            userinfo_url: p.userinfo_url.clone(),
            jwks_url: p.jwks_url.clone(),
        };
        let needs = p.kind != "openid2"
            && (e.authorize_url.is_empty()
                || e.token_url.is_empty()
                || (p.kind == "oidc" && e.jwks_url.is_empty()));
        if needs {
            if p.issuer.is_empty() {
                return Err(Error::InvalidArgument(format!(
                    "login provider {} has no issuer for discovery",
                    p.name
                )));
            }
            let doc = self.discover(social, &p.issuer).await?;
            let pick = |k: &str| doc.get(k).and_then(Value::as_str).map(str::to_string);
            if e.authorize_url.is_empty() {
                e.authorize_url = pick("authorization_endpoint").ok_or_else(|| {
                    Error::Internal("oidc discovery: no authorization_endpoint".into())
                })?;
            }
            if e.token_url.is_empty() {
                e.token_url = pick("token_endpoint")
                    .ok_or_else(|| Error::Internal("oidc discovery: no token_endpoint".into()))?;
            }
            if e.userinfo_url.is_empty() {
                e.userinfo_url = pick("userinfo_endpoint").unwrap_or_default();
            }
            if e.jwks_url.is_empty() {
                e.jwks_url = pick("jwks_uri").unwrap_or_default();
            }
        }
        Ok(e)
    }

    /// Opens a flow: PKCE verifier + nonce stored server-side, `state` handed to the client, and
    /// the authorization URL the browser/webview must open.
    pub async fn start(
        &self,
        social: &Social,
        p: &Provider,
        opts: StartOptions,
        account: Option<AccountId>,
    ) -> Result<Started> {
        let callback = if opts.callback_url.is_empty() {
            p.redirect_urls
                .first()
                .cloned()
                .ok_or_else(|| Error::InvalidArgument("provider has no redirect_urls".into()))?
        } else {
            if !p.redirect_urls.iter().any(|u| u == &opts.callback_url) {
                return Err(Error::InvalidArgument(
                    "callback_url is not one of the provider's redirect_urls".into(),
                ));
            }
            opts.callback_url.clone()
        };
        if !opts.return_url.is_empty()
            && !p
                .return_urls
                .iter()
                .any(|u| opts.return_url.starts_with(u.as_str()))
        {
            return Err(Error::InvalidArgument(
                "return_url is not allowed for this provider".into(),
            ));
        }
        if let Some(u) = &opts.username {
            if u.len() > 32 {
                return Err(Error::InvalidArgument("username too long".into()));
            }
        }
        if serde_json::to_vec(&opts.vars).map(|v| v.len()).unwrap_or(0) > 4096 {
            return Err(Error::InvalidArgument("vars must be ≤ 4 KB".into()));
        }
        let eps = self.endpoints(social, p).await?;
        let state = random_token();
        let verifier = format!("{}{}", random_token(), random_token());
        let nonce = random_token();
        sqlx::query("delete from musubi.auth_flows where expires_at < now()")
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.auth_flows (state, provider, verifier, nonce, callback_url, return_url, account_id, opts, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(secs => $9))",
        )
        .bind(&state)
        .bind(&p.name)
        .bind(&verifier)
        .bind(&nonce)
        .bind(&callback)
        .bind(&opts.return_url)
        .bind(account.map(|a| a.0))
        .bind(serde_json::to_value(&opts).unwrap_or_default())
        .bind(FLOW_TTL_SECS as f64)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        if p.kind == "openid2" {
            let return_to = format!(
                "{callback}{}state={}",
                if callback.contains('?') { '&' } else { '?' },
                enc(&state)
            );
            let sep = if eps.authorize_url.contains('?') {
                '&'
            } else {
                '?'
            };
            let url = format!(
                "{}{sep}openid.ns={}&openid.mode=checkid_setup&openid.return_to={}&openid.realm={}&openid.identity={}&openid.claimed_id={}",
                eps.authorize_url,
                enc(OPENID2_NS),
                enc(&return_to),
                enc(&origin(&callback)?),
                enc(OPENID2_IDENTIFIER_SELECT),
                enc(OPENID2_IDENTIFIER_SELECT),
            );
            return Ok(Started {
                url,
                state,
                nonce,
                expires_in: FLOW_TTL_SECS,
            });
        }
        let mut scopes: Vec<String> = Vec::new();
        if p.kind == "oidc" {
            scopes.push("openid".into());
        }
        for sc in &p.scopes {
            if !scopes.contains(sc) {
                scopes.push(sc.clone());
            }
        }
        let sep = if eps.authorize_url.contains('?') {
            '&'
        } else {
            '?'
        };
        let mut url = format!(
            "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
            eps.authorize_url,
            enc(&p.client_id),
            enc(&callback),
            enc(&scopes.join(" ")),
            enc(&state),
        );
        if p.kind == "oidc" {
            url.push_str(&format!("&nonce={}", enc(&nonce)));
        }
        if p.pkce {
            url.push_str(&format!(
                "&code_challenge={}&code_challenge_method=S256",
                pkce_challenge(&verifier)
            ));
        }
        Ok(Started {
            url,
            state,
            nonce,
            expires_in: FLOW_TTL_SECS,
        })
    }

    /// OpenID 2.0 positive assertion (the `openid.*` query the OP appended to `return_to`) →
    /// verified identity: mode/ns/return_to/op_endpoint/claimed_id checks, signed-field coverage,
    /// then `check_authentication` at the OP (stateless mode — no association needed).
    pub async fn verify_openid2(
        &self,
        social: &Social,
        p: &Provider,
        eps: &Endpoints,
        flow: &Flow,
        params: &HashMap<String, String>,
    ) -> Result<External> {
        let get = |k: &str| params.get(k).map(String::as_str).unwrap_or("");
        let refused = |what: &str| Error::Unauthenticated(format!("openid: {what}"));
        match get("openid.mode") {
            "id_res" => {}
            "cancel" => return Err(refused("the user cancelled at the provider")),
            m => return Err(refused(&format!("unexpected mode `{m}`"))),
        }
        if get("openid.ns") != OPENID2_NS {
            return Err(refused("not an OpenID 2.0 response"));
        }
        let return_to = get("openid.return_to");
        if !return_to.starts_with(flow.callback_url.as_str())
            || !return_to.contains(&format!("state={}", flow.state))
        {
            return Err(refused("return_to does not match this flow"));
        }
        if get("openid.op_endpoint") != eps.authorize_url {
            return Err(refused("op_endpoint is not this provider"));
        }
        let claimed = get("openid.claimed_id");
        if claimed.is_empty() || claimed != get("openid.identity") {
            return Err(refused("claimed_id/identity mismatch"));
        }
        if !claimed.starts_with(&origin(&eps.authorize_url)?) {
            return Err(refused("claimed_id belongs to another provider"));
        }
        let signed: Vec<&str> = get("openid.signed").split(',').collect();
        for f in [
            "op_endpoint",
            "return_to",
            "response_nonce",
            "assoc_handle",
            "claimed_id",
            "identity",
        ] {
            if !signed.contains(&f) {
                return Err(refused(&format!("field `{f}` is not signed")));
            }
        }
        if get("openid.sig").is_empty() {
            return Err(refused("missing signature"));
        }
        let form: Vec<(String, String)> = params
            .iter()
            .filter(|(k, _)| k.starts_with("openid."))
            .map(|(k, v)| {
                if k == "openid.mode" {
                    (k.clone(), "check_authentication".to_string())
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();
        let body = social
            .http()
            .post(&eps.authorize_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("openid check_authentication: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Unavailable(format!("openid check_authentication: {e}")))?
            .text()
            .await
            .map_err(|e| Error::Unavailable(format!("openid check_authentication: {e}")))?;
        if !body.lines().any(|l| l.trim() == "is_valid:true") {
            return Err(refused("assertion rejected by the provider"));
        }
        let subject = if p.claims.subject == "steam_id" {
            claimed
                .rsplit('/')
                .next()
                .filter(|id| id.len() == 17 && id.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| refused("claimed_id is not a SteamID64"))?
                .to_string()
        } else {
            claimed.to_string()
        };
        let mut ext = External {
            subject,
            email: None,
            email_verified: false,
            display_name: None,
            avatar_url: None,
            username: None,
            claims: json!({
                "claimed_id": claimed,
                "op_endpoint": get("openid.op_endpoint"),
                "response_nonce": get("openid.response_nonce"),
            }),
        };
        if p.claims.subject == "steam_id" {
            if let Some((name, avatar)) = social.steam_persona(&ext.subject).await {
                ext.display_name = Some(name);
                ext.avatar_url = avatar;
            }
        }
        Ok(ext)
    }

    /// Consumes the flow (single use); 400 when unknown, expired or for another provider.
    pub async fn take_flow(&self, provider: &str, state: &str) -> Result<Flow> {
        if state.is_empty() || state.len() > 128 {
            return Err(Error::InvalidArgument("state is required".into()));
        }
        type Row = (
            String,
            String,
            String,
            String,
            String,
            Option<Uuid>,
            Value,
            OffsetDateTime,
        );
        let row: Option<Row> = sqlx::query_as(
            "delete from musubi.auth_flows where state = $1
             returning provider, verifier, nonce, callback_url, return_url, account_id, opts, expires_at",
        )
        .bind(state)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some(r) = row else {
            return Err(Error::InvalidArgument(
                "unknown or already used login state".into(),
            ));
        };
        if r.7 < OffsetDateTime::now_utc() {
            return Err(Error::InvalidArgument("login flow expired".into()));
        }
        if r.0 != provider {
            return Err(Error::InvalidArgument(
                "state belongs to another provider".into(),
            ));
        }
        Ok(Flow {
            state: state.to_string(),
            provider: r.0,
            verifier: r.1,
            nonce: r.2,
            callback_url: r.3,
            return_url: r.4,
            account_id: r.5.map(AccountId),
            opts: serde_json::from_value(r.6).unwrap_or_default(),
        })
    }

    /// Authorization code → tokens at the provider's token endpoint.
    pub async fn exchange_code(
        &self,
        social: &Social,
        p: &Provider,
        eps: &Endpoints,
        code: &str,
        flow: &Flow,
    ) -> Result<TokenResponse> {
        if code.is_empty() || code.len() > 4096 {
            return Err(Error::InvalidArgument("code is required".into()));
        }
        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.to_string()),
            ("redirect_uri", flow.callback_url.clone()),
            ("client_id", p.client_id.clone()),
        ];
        if p.pkce {
            form.push(("code_verifier", flow.verifier.clone()));
        }
        let mut req = social
            .http()
            .post(&eps.token_url)
            .header("accept", "application/json");
        if !p.client_secret.is_empty() {
            if p.auth_style == "basic" {
                req = req.basic_auth(&p.client_id, Some(&p.client_secret));
            } else {
                form.push(("client_secret", p.client_secret.clone()));
            }
        }
        let resp = req
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("token endpoint: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Unavailable(format!("token endpoint: {e}")))?;
        let tr: TokenResponse = serde_json::from_str(&body)
            .or_else(|_| form_tokens(&body))
            .map_err(|_| {
                Error::Unauthenticated(format!(
                    "token endpoint returned {status} with an unreadable body"
                ))
            })?;
        if !status.is_success() || tr.error.is_some() {
            return Err(Error::Unauthenticated(format!(
                "token endpoint {}: {} {}",
                status,
                tr.error.clone().unwrap_or_default(),
                tr.error_description.clone().unwrap_or_default()
            )));
        }
        Ok(tr)
    }

    /// Who logged in: oidc → the id_token (nonce-checked when the flow had one) merged with
    /// userinfo when configured; oauth2 → userinfo with the access token.
    pub async fn identify(
        &self,
        social: &Social,
        p: &Provider,
        eps: &Endpoints,
        tokens: &TokenResponse,
        nonce: Option<&str>,
    ) -> Result<External> {
        let mut claims = Value::Object(Default::default());
        if p.kind == "oidc" {
            let id_token = tokens.id_token.as_deref().ok_or_else(|| {
                Error::Unauthenticated("token endpoint returned no id_token".into())
            })?;
            let verified = social
                .verify_id_token(
                    &OidcLoginConfig {
                        jwks_url: eps.jwks_url.clone(),
                        issuer: p.issuer.clone(),
                        issuers: Vec::new(),
                        client_ids: vec![p.client_id.clone()],
                    },
                    id_token,
                )
                .await?;
            if let Some(n) = nonce {
                if verified.extra.get("nonce").and_then(Value::as_str) != Some(n) {
                    return Err(Error::Unauthenticated("id token nonce mismatch".into()));
                }
            }
            let mut obj = verified.extra.clone();
            obj.insert("sub".into(), json!(verified.sub));
            if let Some(e) = &verified.email {
                obj.insert("email".into(), json!(e));
            }
            if let Some(v) = &verified.email_verified {
                obj.insert("email_verified".into(), v.clone());
            }
            claims = Value::Object(obj);
        }
        let want_userinfo = p.kind == "oauth2"
            || (!eps.userinfo_url.is_empty()
                && tokens.access_token.is_some()
                && [
                    &p.claims.display_name,
                    &p.claims.avatar_url,
                    &p.claims.email,
                    &p.claims.username,
                ]
                .iter()
                .any(|k| !k.is_empty() && claims.get(k.split('.').next().unwrap_or("")).is_none()));
        if want_userinfo {
            let at = tokens.access_token.as_deref().ok_or_else(|| {
                Error::Unauthenticated("token endpoint returned no access_token".into())
            })?;
            let info = match self.userinfo(social, &eps.userinfo_url, at).await {
                Ok(v) => v,
                Err(_) if p.kind == "oidc" => Value::Null,
                Err(e) => return Err(e),
            };
            if let (Value::Object(dst), Value::Object(src)) = (&mut claims, info) {
                for (k, v) in src {
                    dst.entry(k).or_insert(v);
                }
            }
        }
        map_claims(&p.claims, &claims)
    }

    pub async fn userinfo(&self, social: &Social, url: &str, access_token: &str) -> Result<Value> {
        if url.is_empty() {
            return Err(Error::InvalidArgument(
                "provider has no userinfo_url".into(),
            ));
        }
        let resp = social
            .http()
            .get(url)
            .bearer_auth(access_token)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("userinfo: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Unauthenticated(format!(
                "userinfo endpoint: {status}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| Error::Unauthenticated(format!("userinfo: {e}")))
    }

    /// Remember what the IdP told us about this identity (email, name, picture) for support.
    pub async fn note_identity(&self, provider: &str, ext: &External) -> Result<()> {
        let meta = json!({
            "email": ext.email, "email_verified": ext.email_verified, "display_name": ext.display_name,
            "avatar_url": ext.avatar_url, "username": ext.username,
            "seen_at": OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        });
        sqlx::query(
            "update musubi.identities set metadata = metadata || $3, verified = verified or $4, updated_at = now()
             where provider = $1 and subject = $2",
        )
        .bind(provider)
        .bind(&ext.subject)
        .bind(meta)
        .bind(ext.email_verified)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn open_flows(&self) -> Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from musubi.auth_flows where expires_at > now()",
        )
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)
    }
}

/// Some OAuth2 servers (GitHub without `Accept: application/json`) answer `a=b&c=d`.
fn form_tokens(body: &str) -> std::result::Result<TokenResponse, ()> {
    if body.contains('{') || !body.contains('=') {
        return Err(());
    }
    let mut tr = TokenResponse::default();
    for pair in body.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = v.replace('+', " ");
        let v = percent_decode(&v);
        match k {
            "access_token" => tr.access_token = Some(v),
            "id_token" => tr.id_token = Some(v),
            "token_type" => tr.token_type = Some(v),
            "refresh_token" => tr.refresh_token = Some(v),
            "expires_in" => tr.expires_in = v.parse().ok(),
            "error" => tr.error = Some(v),
            "error_description" => tr.error_description = Some(v),
            _ => {}
        }
    }
    Ok(tr)
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(h) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenResponse {
    pub access_token: Option<String>,
    pub id_token: Option<String>,
    pub token_type: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_is_s256_of_the_verifier() {
        // RFC 7636 appendix B
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn claims_map_paths_and_fallbacks() {
        let c = json!({"id": 1234, "login": "gamer", "avatar": {"url": "https://x/a.png"}, "verified": "true", "mail": "A@B.co"});
        let m = ClaimsMap {
            subject: "id".into(),
            email: "mail".into(),
            email_verified: "verified".into(),
            display_name: "login".into(),
            avatar_url: "avatar.url".into(),
            username: "missing".into(),
        };
        let e = map_claims(&m, &c).unwrap();
        assert_eq!(e.subject, "1234");
        assert_eq!(e.email.as_deref(), Some("a@b.co"));
        assert!(e.email_verified);
        assert_eq!(e.display_name.as_deref(), Some("gamer"));
        assert_eq!(e.avatar_url.as_deref(), Some("https://x/a.png"));
        assert!(e.username.is_none());
        assert!(map_claims(&ClaimsMap::default(), &json!({"name": "x"})).is_err());
    }

    #[test]
    fn validation_rejects_incomplete_providers() {
        let ok = ProviderInput {
            issuer: "https://idp.example".into(),
            client_id: "c".into(),
            redirect_urls: vec!["https://game/cb".into()],
            ..Default::default()
        };
        assert!(validate("discord", &ok).is_ok());
        assert!(validate("Bad Name", &ok).is_err());
        let mut no_issuer = ok.clone();
        no_issuer.issuer.clear();
        assert!(validate("x", &no_issuer).is_err());
        let mut oauth = ok.clone();
        oauth.kind = "oauth2".into();
        assert!(validate("x", &oauth).is_err());
        oauth.authorize_url = "https://a".into();
        oauth.token_url = "https://t".into();
        oauth.userinfo_url = "https://u".into();
        assert!(validate("x", &oauth).is_ok());
        let mut no_cb = ok.clone();
        no_cb.redirect_urls.clear();
        assert!(validate("x", &no_cb).is_err());
    }
}
