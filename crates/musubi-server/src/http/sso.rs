//! Operator SSO: an OpenID Connect login that mints a short-lived operator key whose role comes
//! from the operator's groups. `GET /v1/admin/sso/login` sends the operator to the provider,
//! `GET /v1/admin/sso/callback` exchanges the code, verifies the ID token and hands the key to the
//! console (`return_to#key=…`) or back as JSON.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use hmac::{Hmac, Mac};
use musubi_core::config::{OidcLoginConfig, OperatorSsoConfig};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::app::AppState;
use crate::http::ApiError;
use crate::keys::Role;

const STATE_TTL_SECS: i64 = 600;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/sso", get(status))
        .route("/v1/admin/sso/login", get(login))
        .route("/v1/admin/sso/callback", get(callback))
}

fn enabled(cfg: &OperatorSsoConfig) -> musubi_core::Result<()> {
    if cfg.issuer.is_empty() || cfg.client_id.is_empty() || cfg.redirect_url.is_empty() {
        return Err(Error::NotFound(
            "operator SSO is not configured ([operators.sso] issuer, client_id, redirect_url)"
                .into(),
        ));
    }
    Ok(())
}

async fn status(State(s): State<AppState>) -> Json<Value> {
    let c = &s.cfg.operators.sso;
    let on = enabled(c).is_ok();
    Json(json!({
        "enabled": on,
        "issuer": on.then(|| c.issuer.clone()),
        "login_url": on.then_some("/v1/admin/sso/login"),
        "roles": c.role_map.values().collect::<std::collections::BTreeSet<_>>(),
        "default_role": (!c.default_role.is_empty()).then(|| c.default_role.clone()),
        "session_hours": c.session_hours,
    }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginQuery {
    /// Console URL that receives `#key=…&role=…&expires_at=…`; omitted = the callback answers JSON.
    return_to: String,
}

async fn login(
    State(s): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> Result<Response, ApiError> {
    let c = &s.cfg.operators.sso;
    enabled(c)?;
    if !q.return_to.is_empty()
        && !(q.return_to.starts_with("https://") || q.return_to.starts_with("http://"))
    {
        return Err(
            Error::InvalidArgument("return_to must be an absolute http(s) URL".into()).into(),
        );
    }
    let authorize = if c.authorize_url.is_empty() {
        endpoint(&s, "authorization_endpoint").await?
    } else {
        c.authorize_url.clone()
    };
    let nonce = uuid::Uuid::now_v7().simple().to_string();
    let state = sign_state(&s.cfg.session.secret, &nonce, &q.return_to);
    let mut scopes = vec!["openid", "email", "profile"];
    for sc in &c.scopes {
        if !scopes.contains(&sc.as_str()) {
            scopes.push(sc);
        }
    }
    let sep = if authorize.contains('?') { '&' } else { '?' };
    let url = format!(
        "{authorize}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}",
        enc(&c.client_id),
        enc(&c.redirect_url),
        enc(&scopes.join(" ")),
        enc(&state),
        enc(&nonce),
    );
    Ok(Redirect::to(&url).into_response())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CallbackQuery {
    code: String,
    state: String,
    error: String,
    error_description: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(s): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, ApiError> {
    let c = &s.cfg.operators.sso;
    enabled(c)?;
    if !q.error.is_empty() {
        return Err(Error::Unauthenticated(format!(
            "provider refused: {} {}",
            q.error, q.error_description
        ))
        .into());
    }
    let (nonce, return_to) = verify_state(&s.cfg.session.secret, &q.state)?;
    if q.code.is_empty() {
        return Err(Error::InvalidArgument("code is required".into()).into());
    }
    let token_url = if c.token_url.is_empty() {
        endpoint(&s, "token_endpoint").await?
    } else {
        c.token_url.clone()
    };
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(Error::internal)?;
    let resp = http
        .post(&token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", q.code.as_str()),
            ("redirect_uri", c.redirect_url.as_str()),
            ("client_id", c.client_id.as_str()),
            ("client_secret", c.client_secret.as_str()),
        ])
        .send()
        .await
        .map_err(|e| Error::Internal(format!("token endpoint: {e}")))?;
    let status = resp.status();
    let tr: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Internal(format!("token endpoint ({status}): {e}")))?;
    let Some(id_token) = tr.id_token else {
        return Err(Error::Unauthenticated(format!(
            "token endpoint {}: {} {}",
            status,
            tr.error.unwrap_or_default(),
            tr.error_description.unwrap_or_default()
        ))
        .into());
    };
    let claims = s
        .logins
        .verify_id_token(
            &OidcLoginConfig {
                jwks_url: c.jwks_url.clone(),
                issuer: c.issuer.clone(),
                issuers: Vec::new(),
                client_ids: vec![c.client_id.clone()],
            },
            &id_token,
        )
        .await?;
    if claims.extra.get("nonce").and_then(Value::as_str) != Some(nonce.as_str()) {
        return Err(Error::Unauthenticated("id token nonce mismatch".into()).into());
    }
    let email = claims.email.clone().unwrap_or_default();
    if !c.allowed_domains.is_empty() {
        let domain = email.rsplit('@').next().unwrap_or("");
        if email.is_empty()
            || !c
                .allowed_domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(domain))
        {
            return Err(Error::PermissionDenied(format!(
                "{} is not in an allowed domain",
                if email.is_empty() {
                    &claims.sub
                } else {
                    &email
                }
            ))
            .into());
        }
    }
    let role = role_of(c, &claims)?;
    let name = if email.is_empty() {
        format!("sso:{}", claims.sub)
    } else {
        format!("sso:{email}")
    };
    let name: String = name.chars().take(64).collect();
    let expires_at = time::OffsetDateTime::now_utc()
        + time::Duration::hours(c.session_hours.clamp(1, 24 * 30) as i64);
    let (caller, secret) = s
        .keys
        .issue_with(&s.db, &name, role, Some(expires_at), Some(&claims.sub))
        .await?;
    if let Some(cl) = &s.cluster {
        let _ = cl.publish(crate::keys::KEYS_TOPIC, &caller.id).await;
    }
    let expires = expires_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    if return_to.is_empty() {
        return Ok(Json(json!({
            "id": caller.id, "name": caller.name, "role": role, "key": secret, "expires_at": expires,
        }))
        .into_response());
    }
    let sep = if return_to.contains('#') { '&' } else { '#' };
    Ok(Redirect::to(&format!(
        "{return_to}{sep}key={}&role={}&expires_at={}",
        enc(&secret),
        role.as_str(),
        enc(&expires)
    ))
    .into_response())
}

async fn endpoint(s: &AppState, key: &str) -> musubi_core::Result<String> {
    let doc = s.logins.discover(&s.cfg.operators.sso.issuer).await?;
    doc.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Internal(format!("oidc discovery: no {key}")))
}

fn rank(r: Role) -> u8 {
    match r {
        Role::Admin => 4,
        Role::Ops => 3,
        Role::Liveops => 2,
        Role::Support => 1,
        Role::Server => 0,
    }
}

/// The most privileged operator role among the operator's mapped groups, else `default_role`.
fn role_of(c: &OperatorSsoConfig, claims: &crate::logins::IdClaims) -> musubi_core::Result<Role> {
    let groups: Vec<String> = match claims.extra.get(&c.role_claim) {
        Some(Value::String(g)) => vec![g.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let mut best: Option<Role> = None;
    for g in &groups {
        if let Some(r) = c.role_map.get(g) {
            let r = Role::parse(r)
                .filter(|r| *r != Role::Server)
                .ok_or_else(|| {
                    Error::Internal(format!(
                        "[operators.sso] role_map: unknown operator role {r}"
                    ))
                })?;
            if best.is_none_or(|b| rank(r) > rank(b)) {
                best = Some(r);
            }
        }
    }
    if let Some(r) = best {
        return Ok(r);
    }
    if c.default_role.is_empty() {
        return Err(Error::PermissionDenied(format!(
            "no operator role for {} (claim {} = {:?})",
            claims.email.as_deref().unwrap_or(&claims.sub),
            c.role_claim,
            groups
        )));
    }
    Role::parse(&c.default_role)
        .filter(|r| *r != Role::Server)
        .ok_or_else(|| {
            Error::Internal("[operators.sso] default_role is not an operator role".into())
        })
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn mac(secret: &str, payload: &str) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    m.update(payload.as_bytes());
    b64(&m.finalize().into_bytes())
}

/// `base64url(nonce|exp|return_to).hmac` — self-contained, so any node can take the callback.
fn sign_state(secret: &str, nonce: &str, return_to: &str) -> String {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + STATE_TTL_SECS;
    let payload = b64(format!("{nonce}|{exp}|{return_to}").as_bytes());
    format!("{payload}.{}", mac(secret, &payload))
}

fn verify_state(secret: &str, state: &str) -> musubi_core::Result<(String, String)> {
    let bad = || Error::InvalidArgument("invalid or expired state".into());
    let (payload, sig) = state.split_once('.').ok_or_else(bad)?;
    let want = mac(secret, payload);
    if want.len() != sig.len()
        || want
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return Err(bad());
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| bad())?;
    let raw = String::from_utf8(raw).map_err(|_| bad())?;
    let mut it = raw.splitn(3, '|');
    let nonce = it.next().unwrap_or_default().to_string();
    let exp: i64 = it.next().and_then(|e| e.parse().ok()).ok_or_else(bad)?;
    let return_to = it.next().unwrap_or_default().to_string();
    if exp < time::OffsetDateTime::now_utc().unix_timestamp() {
        return Err(bad());
    }
    Ok((nonce, return_to))
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_rejects_tampering() {
        let st = sign_state("s3cret", "n1", "https://console/x");
        assert_eq!(
            verify_state("s3cret", &st).unwrap(),
            ("n1".to_string(), "https://console/x".to_string())
        );
        assert!(verify_state("other", &st).is_err());
        let mut forged = st.clone();
        forged.replace_range(0..1, if st.starts_with('A') { "B" } else { "A" });
        assert!(verify_state("s3cret", &forged).is_err());
        assert!(verify_state("s3cret", "nope").is_err());
    }

    #[test]
    fn most_privileged_mapped_group_wins() {
        let mut c = OperatorSsoConfig::default();
        c.role_map.insert("eng".into(), "support".into());
        c.role_map.insert("ops".into(), "ops".into());
        let claims = |groups: Value| crate::logins::IdClaims {
            sub: "u".into(),
            email: Some("a@b.c".into()),
            email_verified: None,
            online_id: None,
            extra: [("groups".to_string(), groups)].into_iter().collect(),
        };
        assert_eq!(
            role_of(&c, &claims(json!(["eng", "ops"]))).unwrap(),
            Role::Ops
        );
        assert_eq!(role_of(&c, &claims(json!("eng"))).unwrap(), Role::Support);
        assert!(role_of(&c, &claims(json!(["nobody"]))).is_err());
        c.default_role = "liveops".into();
        assert_eq!(role_of(&c, &claims(json!([]))).unwrap(), Role::Liveops);
    }
}
