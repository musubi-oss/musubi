//! Passkeys (WebAuthn): a credential is a `passkey` identity whose subject is the base64url
//! credential id and whose metadata holds the public key and sign counter. Challenges travel in a
//! signed 5-minute token (`vars.mfa = "webauthn"`, so no bearer check accepts it).

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use musubi_auth::passkey::{self, Assertion, Attestation, PublicKey};
use musubi_auth::token::Claims;
use musubi_auth::{provider, AuthOptions, Session};
use musubi_core::config::PasskeyConfig;
use musubi_core::{AccountId, Error, SessionId};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{ApiError, Bearer};
use crate::app::AppState;

const CHALLENGE_SECS: i64 = 300;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/account/passkeys/options", post(register_options))
        .route("/v1/account/passkeys", get(list).post(register))
        .route("/v1/account/passkeys/{id}", delete(remove))
        .route("/v1/auth/passkey/options", post(login_options))
        .route("/v1/auth/passkey", post(login))
}

fn cfg(s: &AppState) -> Result<&PasskeyConfig, Error> {
    let c = &s.cfg.social.passkeys;
    if c.rp_id.is_empty() || c.origins.is_empty() {
        return Err(Error::InvalidArgument(
            "passkeys are not configured (social.passkeys.rp_id / origins)".into(),
        ));
    }
    Ok(c)
}

/// Random challenge plus the token that carries it back.
fn challenge(
    s: &AppState,
    account: Option<AccountId>,
    purpose: &str,
) -> Result<(String, String), Error> {
    let ch: [u8; 32] = rand::random();
    let b64 = URL_SAFE_NO_PAD.encode(ch);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut vars = serde_json::Map::new();
    vars.insert("mfa".into(), json!("webauthn"));
    vars.insert("webauthn".into(), json!(b64));
    vars.insert("purpose".into(), json!(purpose));
    let token = s.auth.signer().sign(&Claims {
        sub: account.unwrap_or(AccountId(Uuid::nil())),
        sid: SessionId(Uuid::nil()),
        iat: now,
        exp: now + CHALLENGE_SECS,
        vars,
        name: None,
        scopes: Vec::new(),
    })?;
    Ok((b64, token))
}

fn open_challenge(s: &AppState, token: &str, purpose: &str) -> Result<(Vec<u8>, AccountId), Error> {
    let c = s.auth.signer().verify(token)?;
    if c.vars.get("mfa").and_then(|v| v.as_str()) != Some("webauthn")
        || c.vars.get("purpose").and_then(|v| v.as_str()) != Some(purpose)
    {
        return Err(Error::Unauthenticated(
            "not a passkey challenge token".into(),
        ));
    }
    let ch = c
        .vars
        .get("webauthn")
        .and_then(|v| v.as_str())
        .and_then(|v| URL_SAFE_NO_PAD.decode(v).ok())
        .ok_or_else(|| Error::Unauthenticated("challenge token malformed".into()))?;
    Ok((ch, c.sub))
}

/// `POST /v1/account/passkeys/options` — `PublicKeyCredentialCreationOptions` for the browser
/// plus the `token` to send back with the credential.
async fn register_options(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = cfg(&s)?;
    let (challenge, token) = challenge(&s, Some(me.account_id), "register")?;
    let exclude: Vec<String> = sqlx::query_scalar(
        "select subject from musubi.identities where account_id = $1 and provider = $2",
    )
    .bind(me.account_id.0)
    .bind(provider::PASSKEY)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "token": token,
        "publicKey": {
            "challenge": challenge,
            "rp": { "id": c.rp_id, "name": c.rp_name },
            "user": {
                "id": URL_SAFE_NO_PAD.encode(me.account_id.0.as_bytes()),
                "name": me.account_id.to_string(),
                "displayName": me.account_id.to_string(),
            },
            "pubKeyCredParams": [
                { "type": "public-key", "alg": passkey::ES256 },
                { "type": "public-key", "alg": passkey::RS256 },
            ],
            "excludeCredentials": exclude.iter().map(|id| json!({ "type": "public-key", "id": id })).collect::<Vec<_>>(),
            "authenticatorSelection": { "residentKey": "preferred", "userVerification": "preferred" },
            "attestation": "none",
            "timeout": CHALLENGE_SECS * 1000,
        }
    })))
}

#[derive(Deserialize)]
struct RegisterBody {
    token: String,
    #[serde(default)]
    name: Option<String>,
    credential: Attestation,
}

#[derive(serde::Serialize, Deserialize)]
struct Stored {
    public_key: PublicKey,
    sign_count: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user_verified: bool,
}

/// `POST /v1/account/passkeys` — finish registration; the credential becomes a `passkey` identity.
async fn register(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<RegisterBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = cfg(&s)?;
    let (ch, who) = open_challenge(&s, &b.token, "register")?;
    if who != me.account_id {
        return Err(Error::Unauthenticated("challenge issued to another account".into()).into());
    }
    if b.name.as_ref().is_some_and(|n| n.chars().count() > 64) {
        return Err(Error::InvalidArgument("name over 64 chars".into()).into());
    }
    let reg = passkey::register(&b.credential, &ch, &c.rp_id, &c.origins)?;
    let subject = URL_SAFE_NO_PAD.encode(&reg.credential_id);
    s.auth
        .link_external(me.account_id, provider::PASSKEY, &subject)
        .await?;
    let stored = Stored {
        public_key: reg.public_key,
        sign_count: reg.sign_count,
        name: b.name.clone(),
        user_verified: reg.user_verified,
    };
    sqlx::query(
        "update musubi.identities set metadata = $3, verified = true
         where account_id = $1 and provider = $4 and subject = $2",
    )
    .bind(me.account_id.0)
    .bind(&subject)
    .bind(serde_json::to_value(&stored).map_err(Error::internal)?)
    .bind(provider::PASSKEY)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "id": subject, "name": b.name, "user_verified": stored.user_verified }),
    ))
}

/// `GET /v1/account/passkeys`
async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows: Vec<(String, serde_json::Value, String)> = sqlx::query_as(
        "select subject, metadata,
                to_char(created_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
         from musubi.identities where account_id = $1 and provider = $2 order by created_at",
    )
    .bind(me.account_id.0)
    .bind(provider::PASSKEY)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let keys: Vec<_> = rows
        .into_iter()
        .map(|(id, m, created)| {
            json!({
                "id": id,
                "name": m.get("name").cloned().unwrap_or(serde_json::Value::Null),
                "alg": m.pointer("/public_key/alg").cloned().unwrap_or(serde_json::Value::Null),
                "sign_count": m.get("sign_count").cloned().unwrap_or(json!(0)),
                "last_used_at": m.get("last_used_at").cloned().unwrap_or(serde_json::Value::Null),
                "created_at": created,
            })
        })
        .collect();
    Ok(Json(json!({ "passkeys": keys })))
}

/// `DELETE /v1/account/passkeys/{id}` — same rules as any unlink (an account keeps ≥ 1 identity).
async fn remove(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth.unlink(me.account_id, provider::PASSKEY, &id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// `POST /v1/auth/passkey/options` — `PublicKeyCredentialRequestOptions` (discoverable credentials:
/// no `allowCredentials`, the authenticator picks) plus the challenge token.
async fn login_options(
    State(s): State<AppState>,
    ip: super::ClientIp,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let c = cfg(&s)?;
    let (challenge, token) = challenge(&s, None, "login")?;
    Ok(Json(json!({
        "token": token,
        "publicKey": {
            "challenge": challenge,
            "rpId": c.rp_id,
            "userVerification": "preferred",
            "timeout": CHALLENGE_SECS * 1000,
        }
    })))
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
    credential: Assertion,
    #[serde(flatten)]
    opts: AuthOptions,
}

/// `POST /v1/auth/passkey` — verify the assertion against the stored key and open a session.
async fn login(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(b): Json<LoginBody>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let c = cfg(&s)?;
    let (ch, _) = open_challenge(&s, &b.token, "login")?;
    let raw = URL_SAFE_NO_PAD
        .decode(b.credential.id.as_bytes())
        .map_err(|_| Error::InvalidArgument("credential id is not base64url".into()))?;
    let subject = URL_SAFE_NO_PAD.encode(raw);
    let row: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select account_id, metadata from musubi.identities where provider = $1 and subject = $2",
    )
    .bind(provider::PASSKEY)
    .bind(&subject)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((_, meta)) = row else {
        return Err(Error::Unauthenticated("unknown passkey".into()).into());
    };
    let mut stored: Stored = serde_json::from_value(meta.clone()).map_err(Error::internal)?;
    let count = passkey::verify_assertion(
        &b.credential,
        &stored.public_key,
        stored.sign_count,
        &ch,
        &c.rp_id,
        &c.origins,
    )?;
    stored.sign_count = count;
    let mut m = serde_json::to_value(&stored).map_err(Error::internal)?;
    m["last_used_at"] = json!(time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default());
    sqlx::query(
        "update musubi.identities set metadata = $3, updated_at = now()
         where provider = $1 and subject = $2",
    )
    .bind(provider::PASSKEY)
    .bind(&subject)
    .bind(m)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut opts = b.opts;
    opts.create = false;
    let sess = s
        .auth
        .authenticate_external(provider::PASSKEY, &subject, opts)
        .await?;
    super::auth::authenticated(&s, &sess, provider::PASSKEY, &ip.0).await?;
    Ok(Json(sess))
}
