//! Cross-node session resume. A parked session lives in the memory of the node that served it,
//! so it dies with a crash or a drain. Resumable sessions therefore also keep a small record in
//! Postgres — who they are and what they joined — and a node that gets `?resume=<token>` for a
//! session it does not hold rebuilds it from that record: same session id, same token, the joins
//! replayed, the old copy (if any) stolen over the bus. Buffered outbound messages are not
//! recoverable this way; the client sees the replayed join acknowledgements after `session`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use musubi_core::{AccountId, Error, Result, SessionId};
use musubi_proto::{ClientMessage, UserPresence};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::Mutex;

pub struct ResumeStore {
    pub db: PgPool,
    pub node_id: uuid::Uuid,
    pub group: String,
    pub ttl: Duration,
}

/// What a session needs to be rebuilt elsewhere.
#[derive(Debug, Clone)]
pub struct ResumeRecord {
    pub presence: UserPresence,
    pub channels: Vec<ClientMessage>,
    pub matches: Vec<ClientMessage>,
    /// world name → the subscribe that produced it
    pub worlds: Vec<ClientMessage>,
    /// party id → the join that produced it (parties created by the session are not re-created)
    pub parties: Vec<ClientMessage>,
    pub follows: Vec<AccountId>,
}

#[derive(Default)]
struct State {
    /// channel id → the join that produced it
    channels: HashMap<String, ClientMessage>,
    /// match id → the join that produced it
    matches: HashMap<String, ClientMessage>,
    worlds: HashMap<String, ClientMessage>,
    parties: HashMap<String, ClientMessage>,
    follows: Vec<AccountId>,
}

pub struct ResumeTracker {
    store: Arc<ResumeStore>,
    token_hash: String,
    presence: UserPresence,
    state: Mutex<State>,
    parked: std::sync::atomic::AtomicBool,
    /// Held from reading the state to the row being written, so saves spawned in order cannot
    /// overwrite each other with stale snapshots.
    save_lock: Mutex<()>,
}

/// A fresh resume token: 256 bits from the OS CSPRNG, hex (64 chars). The token is the only
/// credential a reconnecting transport presents (`resume_or_reject`), so it must be
/// unguessable — time-ordered UUIDs are not.
pub fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl ResumeTracker {
    pub fn new(store: Arc<ResumeStore>, token: &str, presence: UserPresence) -> Arc<Self> {
        Arc::new(Self {
            store,
            token_hash: hash_token(token),
            presence,
            state: Mutex::new(State::default()),
            parked: std::sync::atomic::AtomicBool::new(false),
            save_lock: Mutex::new(()),
        })
    }

    pub async fn seed(&self, rec: &ResumeRecord) {
        let mut st = self.state.lock().await;
        st.follows = rec.follows.clone();
    }

    pub async fn joined_channel(self: &Arc<Self>, channel_id: &str, join: ClientMessage) {
        self.state
            .lock()
            .await
            .channels
            .insert(channel_id.to_string(), join);
        self.touch();
    }

    pub async fn left_channel(self: &Arc<Self>, channel_id: &str) {
        self.state.lock().await.channels.remove(channel_id);
        self.touch();
    }

    pub async fn joined_match(self: &Arc<Self>, match_id: &str, join: ClientMessage) {
        self.state
            .lock()
            .await
            .matches
            .insert(match_id.to_string(), join);
        self.touch();
    }

    pub async fn left_match(self: &Arc<Self>, match_id: &str) {
        self.state.lock().await.matches.remove(match_id);
        self.touch();
    }

    pub async fn subscribed_world(self: &Arc<Self>, world: &str, sub: ClientMessage) {
        self.state
            .lock()
            .await
            .worlds
            .insert(world.to_string(), sub);
        self.touch();
    }

    pub async fn unsubscribed_world(self: &Arc<Self>, world: &str) {
        self.state.lock().await.worlds.remove(world);
        self.touch();
    }

    pub async fn joined_party(self: &Arc<Self>, party_id: &str, join: ClientMessage) {
        self.state
            .lock()
            .await
            .parties
            .insert(party_id.to_string(), join);
        self.touch();
    }

    pub async fn left_party(self: &Arc<Self>, party_id: &str) {
        self.state.lock().await.parties.remove(party_id);
        self.touch();
    }

    pub async fn followed(self: &Arc<Self>, ids: &[AccountId]) {
        let mut st = self.state.lock().await;
        for id in ids {
            if !st.follows.contains(id) {
                st.follows.push(*id);
            }
        }
        drop(st);
        self.touch();
    }

    pub async fn unfollowed(self: &Arc<Self>, ids: &[AccountId]) {
        self.state.lock().await.follows.retain(|f| !ids.contains(f));
        self.touch();
    }

    /// Persist the current record (fire and forget; the row is best effort by design).
    pub fn touch(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.save().await {
                tracing::warn!(error = %e, "resumable session save failed");
            }
        });
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.save_lock.lock().await;
        let parked = self.parked.load(std::sync::atomic::Ordering::Relaxed);
        let (channels, matches, worlds, parties, follows) = {
            let st = self.state.lock().await;
            (
                serde_json::to_value(st.channels.values().collect::<Vec<_>>())?,
                serde_json::to_value(st.matches.values().collect::<Vec<_>>())?,
                serde_json::to_value(st.worlds.values().collect::<Vec<_>>())?,
                serde_json::to_value(st.parties.values().collect::<Vec<_>>())?,
                serde_json::to_value(&st.follows)?,
            )
        };
        sqlx::query(
            "insert into musubi.resumable_sessions
               (token_hash, session_id, account_id, username, status, node_id, cluster_group,
                channels, matches, follows, expires_at, updated_at, parked, worlds, parties)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now() + make_interval(secs => $11), now(), $12, $13, $14)
             on conflict (token_hash) do update set
               node_id = excluded.node_id, parked = excluded.parked, status = excluded.status, channels = excluded.channels,
               matches = excluded.matches, follows = excluded.follows, worlds = excluded.worlds,
               parties = excluded.parties,
               expires_at = excluded.expires_at, updated_at = now()",
        )
        .bind(&self.token_hash)
        .bind(self.presence.session_id.0)
        .bind(self.presence.account_id.0)
        .bind(&self.presence.username)
        .bind(&self.presence.status)
        .bind(self.store.node_id)
        .bind(&self.store.group)
        .bind(channels)
        .bind(matches)
        .bind(follows)
        .bind(self.store.ttl.as_secs_f64())
        .bind(parked)
        .bind(worlds)
        .bind(parties)
        .execute(&self.store.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Transport lost (true) or re-attached (false). Only a parked record may be taken over by
    /// another node while this one is alive.
    pub async fn set_parked(&self, parked: bool) {
        self.parked
            .store(parked, std::sync::atomic::Ordering::Relaxed);
        let _guard = self.save_lock.lock().await;
        if let Err(e) = sqlx::query(
            "update musubi.resumable_sessions set parked = $3, updated_at = now()
             where token_hash = $1 and node_id = $2",
        )
        .bind(&self.token_hash)
        .bind(self.store.node_id)
        .bind(parked)
        .execute(&self.store.db)
        .await
        {
            tracing::warn!(error = %e, "resumable session park flag failed");
        }
    }

    /// The session ended for good on this node: drop the record unless another node took it over.
    pub fn forget(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "delete from musubi.resumable_sessions where token_hash = $1 and node_id = $2",
            )
            .bind(&me.token_hash)
            .bind(me.store.node_id)
            .execute(&me.store.db)
            .await;
        });
    }
}

/// session_id, account_id, username, status, node_id, channels, matches, follows, parked, worlds, parties
type Row = (
    uuid::Uuid,
    uuid::Uuid,
    String,
    Option<String>,
    uuid::Uuid,
    serde_json::Value,
    serde_json::Value,
    serde_json::Value,
    bool,
    serde_json::Value,
    serde_json::Value,
);

impl ResumeStore {
    /// Load the record behind `token` if it is still valid. Returns the node that last held it and
    /// whether the session was parked there.
    pub async fn load(&self, token: &str) -> Result<Option<(ResumeRecord, uuid::Uuid, bool)>> {
        let row: Option<Row> = sqlx::query_as(
            "select session_id, account_id, username, status, node_id, channels, matches, follows, parked,
                    worlds, parties
             from musubi.resumable_sessions
             where token_hash = $1 and cluster_group = $2 and expires_at > now()",
        )
        .bind(hash_token(token))
        .bind(&self.group)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some((
            sid,
            acc,
            username,
            status,
            node,
            channels,
            matches,
            follows,
            parked,
            worlds,
            parties,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some((
            ResumeRecord {
                presence: UserPresence {
                    account_id: AccountId(acc),
                    session_id: SessionId(sid),
                    username,
                    status,
                    rich: None,
                    invisible: false,
                },
                channels: serde_json::from_value(channels).unwrap_or_default(),
                matches: serde_json::from_value(matches).unwrap_or_default(),
                worlds: serde_json::from_value(worlds).unwrap_or_default(),
                parties: serde_json::from_value(parties).unwrap_or_default(),
                follows: serde_json::from_value(follows).unwrap_or_default(),
            },
            node,
            parked,
        )))
    }

    /// Retention: expired records.
    pub async fn purge_expired(db: &PgPool) -> Result<u64> {
        let r = sqlx::query("delete from musubi.resumable_sessions where expires_at <= now()")
            .execute(db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected())
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn new_token_is_random_hex_256_bits() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        // no shared time-ordered prefix (UUIDv7 tokens shared their first 12+ hex digits)
        assert_ne!(&a[..12], &b[..12]);
        assert_eq!(hash_token(&a).len(), 64);
        assert_ne!(hash_token(&a), hash_token(&b));
    }
}
