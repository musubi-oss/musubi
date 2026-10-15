//! Shared handshake for every transport: token → presence + encoding, then `ConnectParams`.

use musubi_core::{Error, Result, SessionId};
use musubi_proto::{Encoding, UserPresence};
use serde::Deserialize;

use crate::app::AppState;
use crate::realtime::ConnectParams;
use musubi_transport::Limits;

/// What a client sends to open a realtime session (query string on WS, first frame on QUIC).
#[derive(Debug, Clone, Deserialize)]
pub struct Hello {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// Ask for a `session` message with a resume token (opt-in: keeps the wire protocol unchanged
    /// for clients that do not reconnect).
    #[serde(default)]
    pub resumable: bool,
    /// Re-attach to a parked session instead of opening a new one (no `token` needed).
    #[serde(default)]
    pub resume: Option<String>,
}

pub fn encoding_of(hello: &Hello) -> Result<Encoding> {
    match hello.format.as_deref() {
        None => Ok(Encoding::Json),
        Some(f) => Encoding::parse(f)
            .ok_or_else(|| Error::InvalidArgument("format must be json or binary".into())),
    }
}

pub async fn authorize(s: &AppState, hello: &Hello) -> Result<(UserPresence, Encoding)> {
    let me = s.auth.verify_access(&hello.token)?;
    let encoding = encoding_of(hello)?;
    let username: String = sqlx::query_scalar(
        "select username from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::Unauthenticated("account gone".into()))?;
    // Each socket is its own realtime session, distinct from the auth session that issued the token.
    Ok((
        UserPresence {
            account_id: me.account_id,
            session_id: SessionId::new(),
            username,
            status: hello.status.clone(),
        },
        encoding,
    ))
}

pub fn params(s: &AppState, presence: UserPresence, encoding: Encoding) -> ConnectParams {
    ConnectParams {
        presence,
        encoding,
        resumable: false,
        resume_ttl: std::time::Duration::from_secs(s.cfg.realtime.resume_ttl_secs),
        limits: Limits::default(),
        rate: s.limits.clone(),
        chat: s.chat.clone(),
        matchmaker: s.matchmaker.clone(),
        parties: s.parties.clone(),
        relayed: s.relayed.clone(),
        matches: s.matches.clone(),
        worlds: s.worlds.clone(),
        runtime: s.runtime.clone(),
        fanout: s.fanout.clone(),
    }
}
