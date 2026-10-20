//! Generic OAuth2 / OIDC provider catalog: `/v1/auth/providers/*` (players), link flows under
//! `/v1/account/link/providers/*`, admin CRUD under `/v1/admin/auth/providers/*`.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_auth::AuthOptions;
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;
use crate::auth_providers::{External, Flow, Provider, ProviderInput, StartOptions};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/providers", get(list_public))
        .route("/v1/auth/providers/{name}/start", post(start))
        .route("/v1/auth/providers/{name}/login", get(login_redirect))
        .route("/v1/auth/providers/{name}/callback", get(callback))
        .route("/v1/auth/providers/{name}/exchange", post(exchange))
        .route("/v1/auth/providers/{name}/token", post(token_login))
        .route("/v1/account/link/providers/{name}/start", post(link_start))
        .route("/v1/account/link/providers/{name}/token", post(link_token))
        .route("/v1/admin/auth/providers", get(admin_list))
        .route(
            "/v1/admin/auth/providers/{name}",
            get(admin_get).put(admin_put).delete(admin_delete),
        )
        .route("/v1/admin/auth/providers/{name}/test", post(admin_test))
}

/// `GET /v1/auth/providers` — enabled providers for the login screen (no token).
async fn list_public(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let all = s.auth_providers.list().await?;
    let providers: Vec<Value> = all
        .iter()
        .filter(|p| p.enabled)
        .map(Provider::public)
        .collect();
    Ok(Json(json!({ "providers": providers })))
}

/// `POST /v1/auth/providers/{name}/start {return_url?, callback_url?, create?, username?, vars?}`
/// → `{url, state, expires_in}`: open `url` in the browser/webview; the callback (or
/// `/exchange` with the code) finishes the login.
async fn start(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(name): Path<String>,
    Json(opts): Json<StartOptions>,
) -> Result<Json<Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let p = s.auth_providers.enabled(&name).await?;
    let st = s.auth_providers.start(&s.logins, &p, opts, None).await?;
    Ok(Json(
        json!({ "url": st.url, "state": st.state, "expires_in": st.expires_in }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoginQuery {
    return_url: String,
    callback_url: String,
}

/// `GET /v1/auth/providers/{name}/login?return_url=` — browser entry point: 302 to the IdP.
async fn login_redirect(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(name): Path<String>,
    Query(q): Query<LoginQuery>,
) -> Result<Response, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let p = s.auth_providers.enabled(&name).await?;
    let opts = StartOptions {
        return_url: q.return_url,
        callback_url: q.callback_url,
        ..Default::default()
    };
    let st = s.auth_providers.start(&s.logins, &p, opts, None).await?;
    Ok(Redirect::to(&st.url).into_response())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CallbackQuery {
    code: String,
    state: String,
    error: String,
    error_description: String,
}

/// `GET /v1/auth/providers/{name}/callback?code&state` — the IdP sends the browser here.
/// With a `return_url` on the flow the result travels in the fragment
/// (`#access_token=…&refresh_token=…&created=…` or `#linked=1`, `#error=…`), else JSON.
async fn callback(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(name): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let p = s.auth_providers.enabled(&name).await?;
    let pick = |k: &str| raw.get(k).cloned().unwrap_or_default();
    let q = CallbackQuery {
        code: pick("code"),
        state: pick("state"),
        error: pick("error"),
        error_description: pick("error_description"),
    };
    let flow = s.auth_providers.take_flow(&name, &q.state).await?;
    let return_url = flow.return_url.clone();
    let res = async {
        if !q.error.is_empty() {
            return Err(Error::Unauthenticated(format!(
                "provider refused: {} {}",
                q.error, q.error_description
            )));
        }
        finish(&s, &p, flow, &q.code, &ip.0, Some(&raw)).await
    }
    .await;
    if return_url.is_empty() {
        return Ok(Json(res?).into_response());
    }
    let sep = if return_url.contains('#') { '&' } else { '#' };
    let frag = match res {
        Ok(v) => {
            if v.get("linked").is_some() {
                "linked=1".to_string()
            } else {
                format!(
                    "access_token={}&refresh_token={}&created={}",
                    enc(v["access_token"].as_str().unwrap_or("")),
                    enc(v["refresh_token"].as_str().unwrap_or("")),
                    v["created"].as_bool().unwrap_or(false)
                )
            }
        }
        Err(e) => format!("error={}", enc(&e.to_string())),
    };
    Ok(Redirect::to(&format!("{return_url}{sep}{frag}")).into_response())
}

#[derive(Deserialize)]
struct ExchangeReq {
    code: String,
    state: String,
}

/// `POST /v1/auth/providers/{name}/exchange {code, state}` — native apps that catch the redirect
/// themselves hand the code over; same result as the callback, as JSON.
async fn exchange(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(name): Path<String>,
    Json(r): Json<ExchangeReq>,
) -> Result<Json<Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let p = s.auth_providers.enabled(&name).await?;
    let flow = s.auth_providers.take_flow(&name, &r.state).await?;
    Ok(Json(finish(&s, &p, flow, &r.code, &ip.0, None).await?))
}

async fn finish(
    s: &AppState,
    p: &Provider,
    flow: Flow,
    code: &str,
    ip: &str,
    openid: Option<&HashMap<String, String>>,
) -> musubi_core::Result<Value> {
    let eps = s.auth_providers.endpoints(&s.logins, p).await?;
    let (ext, provider) = if p.kind == "openid2" {
        let params = openid.ok_or_else(|| {
            Error::InvalidArgument("openid2 providers finish through the browser callback".into())
        })?;
        let ext = s
            .auth_providers
            .verify_openid2(&s.logins, p, &eps, &flow, params)
            .await?;
        let provider = if p.claims.subject == "steam_id" {
            musubi_auth::provider::STEAM.to_string()
        } else {
            crate::logins::oidc_provider(&p.name)
        };
        (ext, provider)
    } else {
        let tokens = s
            .auth_providers
            .exchange_code(&s.logins, p, &eps, code, &flow)
            .await?;
        let ext = s
            .auth_providers
            .identify(&s.logins, p, &eps, &tokens, Some(&flow.nonce))
            .await?;
        (ext, crate::logins::oidc_provider(&p.name))
    };
    if let Some(account) = flow.account_id {
        s.auth
            .link_external(account, &provider, &ext.subject)
            .await?;
        s.auth_providers.note_identity(&provider, &ext).await?;
        s.events
            .emit(
                "account.linked",
                Some(account),
                json!({ "provider": provider, "subject": ext.subject, "catalog": p.name }),
            )
            .await;
        return Ok(json!({ "linked": provider, "subject": ext.subject, "email": ext.email }));
    }
    let opts = AuthOptions {
        create: flow.opts.create.unwrap_or(true),
        username: flow.opts.username.clone().or_else(|| ext.username.clone()),
        vars: flow.opts.vars.clone(),
        captcha: None,
    };
    login(s, p, &provider, ext, opts, ip).await
}

async fn login(
    s: &AppState,
    p: &Provider,
    provider: &str,
    ext: External,
    mut opts: AuthOptions,
    ip: &str,
) -> musubi_core::Result<Value> {
    if !p.allow_signup {
        opts.create = false;
    }
    let sess = match s
        .auth
        .authenticate_external(provider, &ext.subject, opts.clone())
        .await
    {
        Ok(sess) => sess,
        Err(Error::InvalidArgument(_)) if opts.username.is_some() => {
            // a username taken from the IdP may collide or break the policy: fall back to generated
            opts.username = None;
            s.auth
                .authenticate_external(provider, &ext.subject, opts)
                .await?
        }
        Err(e) => return Err(e),
    };
    if sess.created {
        let patch = musubi_account::UpdateAccount {
            display_name: ext.display_name.clone(),
            avatar_url: ext.avatar_url.clone(),
            ..Default::default()
        };
        if patch.display_name.is_some() || patch.avatar_url.is_some() {
            let _ = s.accounts.update(sess.account_id, patch).await;
        }
    }
    let _ = s.auth_providers.note_identity(provider, &ext).await;
    super::auth::authenticated(s, &sess, provider, ip)
        .await
        .map_err(|e| e.0)?;
    s.events
        .emit(
            "auth.provider_login",
            Some(sess.account_id),
            json!({ "provider": provider, "catalog": p.name, "created": sess.created }),
        )
        .await;
    let mut v = serde_json::to_value(&sess).unwrap_or_default();
    v["provider"] = json!(provider);
    v["email"] = json!(ext.email);
    Ok(v)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TokenReq {
    id_token: Option<String>,
    access_token: Option<String>,
    #[serde(flatten)]
    opts: AuthOptions,
}

/// `POST /v1/auth/providers/{name}/token {id_token | access_token, create?, username?, vars?}` —
/// the client already holds a token from the provider's native SDK: oidc providers take the
/// id_token (verified against JWKS), oauth2 ones the access token (checked at userinfo).
async fn token_login(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(name): Path<String>,
    Json(r): Json<TokenReq>,
) -> Result<Json<Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let p = s.auth_providers.enabled(&name).await?;
    let ext = identify_token(&s, &p, r.id_token, r.access_token).await?;
    let provider = crate::logins::oidc_provider(&p.name);
    Ok(Json(login(&s, &p, &provider, ext, r.opts, &ip.0).await?))
}

async fn identify_token(
    s: &AppState,
    p: &Provider,
    id_token: Option<String>,
    access_token: Option<String>,
) -> musubi_core::Result<External> {
    if p.kind == "openid2" {
        return Err(Error::InvalidArgument(
            "openid2 providers issue no tokens; use the browser flow".into(),
        ));
    }
    if p.kind == "oidc" && id_token.is_none() {
        return Err(Error::InvalidArgument("id_token is required".into()));
    }
    if p.kind == "oauth2" && access_token.is_none() {
        return Err(Error::InvalidArgument("access_token is required".into()));
    }
    let eps = s.auth_providers.endpoints(&s.logins, p).await?;
    let tokens = crate::auth_providers::TokenResponse {
        id_token,
        access_token,
        ..Default::default()
    };
    s.auth_providers
        .identify(&s.logins, p, &eps, &tokens, None)
        .await
}

/// `POST /v1/account/link/providers/{name}/start` — like `/start` but the finished flow links
/// the IdP identity to the caller instead of logging in (409 when it belongs to someone else).
async fn link_start(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(name): Path<String>,
    Json(opts): Json<StartOptions>,
) -> Result<Json<Value>, ApiError> {
    let p = s.auth_providers.enabled(&name).await?;
    let st = s
        .auth_providers
        .start(&s.logins, &p, opts, Some(me.account_id))
        .await?;
    Ok(Json(
        json!({ "url": st.url, "state": st.state, "expires_in": st.expires_in }),
    ))
}

/// `POST /v1/account/link/providers/{name}/token {id_token | access_token}` — link with a token
/// the client already holds.
async fn link_token(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(name): Path<String>,
    Json(r): Json<TokenReq>,
) -> Result<Json<Value>, ApiError> {
    let p = s.auth_providers.enabled(&name).await?;
    let ext = identify_token(&s, &p, r.id_token, r.access_token).await?;
    let provider = crate::logins::oidc_provider(&p.name);
    s.auth
        .link_external(me.account_id, &provider, &ext.subject)
        .await?;
    s.auth_providers.note_identity(&provider, &ext).await?;
    s.events
        .emit(
            "account.linked",
            Some(me.account_id),
            json!({ "provider": provider, "subject": ext.subject, "catalog": p.name }),
        )
        .await;
    Ok(Json(
        json!({ "ok": true, "provider": provider, "subject": ext.subject }),
    ))
}

async fn admin_list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let providers = s.auth_providers.list().await?;
    let open_flows = s.auth_providers.open_flows().await?;
    Ok(Json(
        json!({ "providers": providers, "open_flows": open_flows }),
    ))
}

async fn admin_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Provider>, ApiError> {
    Ok(Json(s.auth_providers.get(&name).await?))
}

/// `PUT /v1/admin/auth/providers/{name}` — create or replace; omit `client_secret` to keep it.
async fn admin_put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
    Json(p): Json<ProviderInput>,
) -> Result<Json<Provider>, ApiError> {
    let saved = s.auth_providers.put(&name, p).await?;
    s.events
        .emit("auth.provider_updated", None, json!({ "name": name }))
        .await;
    Ok(Json(saved))
}

async fn admin_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let deleted = s.auth_providers.delete(&name).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// `POST /v1/admin/auth/providers/{name}/test` — resolves endpoints (runs discovery) and fetches
/// the JWKS for oidc providers; reports what a login would use.
async fn admin_test(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = s.auth_providers.get(&name).await?;
    let eps = s.auth_providers.endpoints(&s.logins, &p).await?;
    let mut keys = Value::Null;
    if p.kind == "oidc" && !eps.jwks_url.is_empty() {
        let set: Value = s
            .logins
            .http()
            .get(&eps.jwks_url)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("jwks: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Unavailable(format!("jwks: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Unavailable(format!("jwks: {e}")))?;
        keys = json!(set["keys"].as_array().map(|k| k.len()).unwrap_or(0));
    }
    let probe = s
        .auth_providers
        .start(&s.logins, &p, StartOptions::default(), None)
        .await?;
    let _ = s.auth_providers.take_flow(&p.name, &probe.state).await;
    Ok(Json(json!({
        "ok": true, "endpoints": eps, "jwks_keys": keys, "authorize_url_example": probe.url,
    })))
}

fn enc(s: &str) -> String {
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
