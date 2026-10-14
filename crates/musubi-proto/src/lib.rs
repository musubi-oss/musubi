//! Realtime wire protocol.
//!
//! Every realtime frame is an [`Envelope`]: an optional correlation id (`cid`) plus one
//! [`ClientMessage`] or [`ServerMessage`] under `msg`, externally tagged:
//! `{"cid":"1","msg":{"status_update":{"status":"afk"}}}`, unit variants as `{"msg":"ping"}`.
//! Two encodings share the same schema: JSON (text frames, debuggable) and postcard (binary
//! frames, compact, not self-describing — hence no flatten/internal/adjacent tags and no
//! `skip_serializing_if`: every field is always present on the wire). The client
//! picks one at connect time (`?format=json|binary`); the server always answers in kind.

use bytes::Bytes;
use musubi_core::{AccountId, SessionId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub mod realtime;
pub use realtime::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Json,
    Postcard,
}

impl Encoding {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "json" => Some(Self::Json),
            "binary" | "postcard" => Some(Self::Postcard),
            _ => None,
        }
    }

    pub fn encode<T: Serialize>(self, v: &T) -> Result<Bytes, ProtoError> {
        match self {
            Encoding::Json => serde_json::to_vec(v)
                .map(Bytes::from)
                .map_err(|e| ProtoError(e.to_string())),
            Encoding::Postcard => postcard::to_allocvec(v)
                .map(Bytes::from)
                .map_err(|e| ProtoError(e.to_string())),
        }
    }

    pub fn decode<T: DeserializeOwned>(self, b: &[u8]) -> Result<T, ProtoError> {
        match self {
            Encoding::Json => serde_json::from_slice(b).map_err(|e| ProtoError(e.to_string())),
            Encoding::Postcard => postcard::from_bytes(b).map_err(|e| ProtoError(e.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoError(pub String);

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ProtoError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope<M> {
    /// Client-chosen correlation id, echoed on the reply.
    #[serde(default)]
    pub cid: Option<String>,
    pub msg: M,
}

/// A connected user as seen by other users.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct UserPresence {
    pub account_id: AccountId,
    pub session_id: SessionId,
    pub username: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_and_postcard_roundtrip() {
        let env = Envelope {
            cid: Some("1".into()),
            msg: ClientMessage::StatusUpdate {
                status: Some("afk".into()),
            },
        };
        for enc in [Encoding::Json, Encoding::Postcard] {
            let b = enc.encode(&env).unwrap();
            let back: Envelope<ClientMessage> = enc.decode(&b).unwrap();
            assert_eq!(back, env);
        }
    }

    #[test]
    fn json_shape_is_externally_tagged() {
        let env = Envelope {
            cid: None,
            msg: ClientMessage::Ping,
        };
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"cid":null,"msg":"ping"}"#
        );
        let none: Envelope<ServerMessage> = Envelope {
            cid: None,
            msg: ServerMessage::StatusPresenceEvent {
                joins: vec![],
                leaves: vec![],
            },
        };
        let b = Encoding::Postcard.encode(&none).unwrap();
        assert_eq!(
            Encoding::Postcard
                .decode::<Envelope<ServerMessage>>(&b)
                .unwrap(),
            none
        );
        let env: Envelope<ClientMessage> =
            serde_json::from_str(r#"{"cid":"7","msg":{"status_follow":{"account_ids":[]}}}"#)
                .unwrap();
        assert_eq!(env.cid.as_deref(), Some("7"));
    }
}
