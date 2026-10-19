//! Session baggage (Hathora/Rivet parity): free-form per-session vars attached after login —
//! `party`, `region`, `shard`, a UI state blob — distinct from account metadata (lives across
//! sessions) and from storage (bigger, versioned). Login vars (`POST /v1/auth/* {vars}`) seed
//! the bag; `PATCH /v1/session/vars {set, unset}` edits it and hands back an access token that
//! carries the new bag, so game servers and modules see it in the token without a lookup;
//! `POST /v1/auth/refresh` re-reads it too. The server stamps bags with
//! `PATCH /v1/server/accounts/{id}/sessions/{sid}/vars` (the client picks that up on refresh or
//! via `GET /v1/session/vars`). Keys `mfa`, `queue`, `pending_vars` are the auth flow's own.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result, SessionId};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

const MAX_KEYS: usize = 64;
const MAX_PATCH_BYTES: usize = 8 * 1024;
const RESERVED: &[&str] = &["mfa", "queue", "pending_vars"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/session/vars", get(my_vars).patch(patch_my_vars))
        .route(
            "/v1/server/accounts/{id}/sessions/{sid}/vars",
            get(server_vars).patch(server_patch_vars),
        )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VarsPatch {
    set: Map<String, Value>,
    unset: Vec<String>,
}

fn valid_key(k: &str) -> Result<()> {
    if k.is_empty()
        || k.len() > 64
        || !k
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(Error::InvalidArgument(format!(
            "session var key {k:?}: [A-Za-z0-9_.-]{{1,64}}"
        )));
    }
    if RESERVED.contains(&k) {
        return Err(Error::InvalidArgument(format!(
            "session var {k:?} is reserved by the auth flow"
        )));
    }
    Ok(())
}

async fn apply(
    s: &AppState,
    account: AccountId,
    session: SessionId,
    p: VarsPatch,
    by: &str,
) -> Result<(Map<String, Value>, String)> {
    if p.set.is_empty() && p.unset.is_empty() {
        return Err(Error::InvalidArgument("set or unset something".into()));
    }
    for k in p.set.keys().chain(p.unset.iter()) {
        valid_key(k)?;
    }
    if serde_json::to_vec(&p.set)
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
        > MAX_PATCH_BYTES
    {
        return Err(Error::InvalidArgument(format!(
            "set exceeds {MAX_PATCH_BYTES} bytes"
        )));
    }
    let current = s.auth.session_vars(account, session).await?;
    let total = current
        .keys()
        .chain(p.set.keys())
        .filter(|k| !p.unset.contains(*k))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if total > MAX_KEYS {
        return Err(Error::InvalidArgument(format!(
            "session vars: at most {MAX_KEYS} keys ({total} after patch)"
        )));
    }
    let set_keys: Vec<&String> = p.set.keys().collect();
    let (vars, token) = s
        .auth
        .set_session_vars(account, session, p.set.clone(), &p.unset)
        .await?;
    s.events
        .emit(
            "session.vars_updated",
            Some(account),
            json!({ "session_id": session, "set": set_keys, "unset": p.unset, "by": by }),
        )
        .await;
    Ok((vars, token))
}

async fn my_vars(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let vars = s.auth.session_vars(me.account_id, me.session_id).await?;
    Ok(Json(json!({ "session_id": me.session_id, "vars": vars })))
}

async fn patch_my_vars(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(p): Json<VarsPatch>,
) -> Result<Json<Value>, ApiError> {
    let (vars, token) = apply(&s, me.account_id, me.session_id, p, "client").await?;
    Ok(Json(
        json!({ "session_id": me.session_id, "vars": vars, "access_token": token }),
    ))
}

fn ids((id, sid): (String, String)) -> Result<(AccountId, SessionId)> {
    let a = Uuid::parse_str(&id).map_err(|_| Error::InvalidArgument("account id".into()))?;
    let s = Uuid::parse_str(&sid).map_err(|_| Error::InvalidArgument("session id".into()))?;
    Ok((AccountId(a), SessionId(s)))
}

async fn server_vars(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(p): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let (account, session) = ids(p)?;
    let vars = s.auth.session_vars(account, session).await?;
    Ok(Json(
        json!({ "account_id": account, "session_id": session, "vars": vars }),
    ))
}

async fn server_patch_vars(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(p): Path<(String, String)>,
    Json(body): Json<VarsPatch>,
) -> Result<Json<Value>, ApiError> {
    let (account, session) = ids(p)?;
    let (vars, _) = apply(&s, account, session, body, "server").await?;
    Ok(Json(
        json!({ "account_id": account, "session_id": session, "vars": vars }),
    ))
}
