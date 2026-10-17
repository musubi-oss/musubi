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

/// Account → username for the handshake, kept `USERNAME_TTL`. Ten thousand sockets opening
/// together used to queue ten thousand reads on the pool and time out; a name does not change,
/// and a deleted account is refused within the TTL (its tokens outlive it about as long anyway).
static USERNAMES: std::sync::OnceLock<dashmap::DashMap<uuid::Uuid, (String, std::time::Instant)>> =
    std::sync::OnceLock::new();
const USERNAME_TTL: std::time::Duration = std::time::Duration::from_secs(60);

async fn username_of(s: &AppState, account: uuid::Uuid) -> Result<String> {
    let cache = USERNAMES.get_or_init(dashmap::DashMap::new);
    let now = std::time::Instant::now();
    if let Some(hit) = cache.get(&account) {
        if now.duration_since(hit.1) < USERNAME_TTL {
            return Ok(hit.0.clone());
        }
    }
    let username: String = sqlx::query_scalar(
        "select username from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::Unauthenticated("account gone".into()))?;
    if cache.len() > 65_536 {
        cache.retain(|_, (_, at)| now.duration_since(*at) < USERNAME_TTL);
    }
    cache.insert(account, (username.clone(), now));
    Ok(username)
}

pub async fn authorize(s: &AppState, hello: &Hello) -> Result<(UserPresence, Encoding)> {
    let me = s.auth.verify_access(&hello.token)?;
    let encoding = encoding_of(hello)?;
    let username = match me.username {
        Some(name) => name,
        None => username_of(s, me.account_id.0).await?,
    };
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
        resume_store: resume_store(s),
        resume_token: None,
        restore: None,
        limits: Limits::default(),
        rate: s.limits.clone(),
        chat: s.chat.clone(),
        notify: s.notify.clone(),
        social: s.social.clone(),
        ratings: s.ratings.clone(),
        storage: s.storage.clone(),
        matchmaker: s.matchmaker.clone(),
        parties: s.parties.clone(),
        relayed: s.relayed.clone(),
        matches: s.matches.clone(),
        worlds: s.worlds.clone(),
        banned: s.banned.clone(),
        anticheat: s.anticheat.clone(),
        runtime: s.runtime.clone(),
        fanout: s.fanout.clone(),
    }
}

fn resume_store(s: &AppState) -> Option<std::sync::Arc<crate::realtime::resume::ResumeStore>> {
    if s.cfg.realtime.resume_ttl_secs == 0 {
        return None;
    }
    Some(std::sync::Arc::new(crate::realtime::resume::ResumeStore {
        db: s.db.clone(),
        node_id: s
            .cluster
            .as_ref()
            .map(|c| c.node_id().0)
            .unwrap_or_else(uuid::Uuid::nil),
        group: s.cfg.node.cluster_group.clone(),
        ttl: std::time::Duration::from_secs(s.cfg.realtime.resume_ttl_secs),
    }))
}

/// `?resume=<token>`: re-attach to the parked session if this node holds it, otherwise rebuild it
/// from its record (taking it over from whichever node last held it), or tell the client why not.
pub async fn resume_any(
    s: &AppState,
    token: &str,
    conn: Box<dyn musubi_transport::Connection>,
    encoding: Encoding,
) {
    let conn = match s
        .hub
        .resume(token, crate::realtime::hub::Resumed { conn, encoding })
    {
        Ok(()) => return,
        Err(conn) => conn,
    };
    let loaded = match resume_store(s) {
        Some(store) => store.load(token).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "resume record lookup failed");
            None
        }),
        None => None,
    };
    let Some((rec, holder, parked)) = loaded else {
        crate::realtime::resume_or_reject(&s.hub, token, conn, encoding).await;
        return;
    };
    // a live session on a live node keeps its token to itself; only a parked one or one whose
    // node is gone can be taken over
    if !parked && holder_alive(s, holder).await {
        crate::realtime::resume_or_reject(&s.hub, token, conn, encoding).await;
        return;
    }
    s.fanout.steal_session(rec.presence.session_id);
    let mut params = params(s, rec.presence.clone(), encoding);
    params.resumable = true;
    params.resume_token = Some(token.to_string());
    params.restore = Some(rec);
    tracing::info!(session = %params.presence.session_id, "resumable session rebuilt from its record");
    crate::realtime::serve(s.hub.clone(), conn, params).await;
}

async fn holder_alive(s: &AppState, holder: uuid::Uuid) -> bool {
    match &s.cluster {
        Some(c) => {
            if c.node_id().0 == holder {
                return true;
            }
            match c.nodes().await {
                Ok(nodes) => nodes.iter().any(|n| n.id.0 == holder && n.alive),
                Err(_) => true,
            }
        }
        None => true,
    }
}
