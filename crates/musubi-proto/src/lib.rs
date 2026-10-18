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

/// Free-form JSON (`serde_json::Value`) on a binary wire. Postcard is not self-describing and
/// cannot decode `Value` (it needs `deserialize_any`), so in a non-human-readable encoding the
/// value travels as its JSON text in a string; JSON encodings pass it through untouched.
/// JSON kept as text: serialized once when a value changes, then copied verbatim into every
/// frame that carries it (human-readable encodings emit it raw, binary ones as a string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonText(pub std::sync::Arc<str>);

impl JsonText {
    pub fn from_value(v: &serde_json::Value) -> Self {
        Self(v.to_string().into())
    }
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::from_str(&self.0).unwrap_or(serde_json::Value::Null)
    }
}

impl From<serde_json::Value> for JsonText {
    fn from(v: serde_json::Value) -> Self {
        Self::from_value(&v)
    }
}

impl Serialize for JsonText {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            let raw = serde_json::value::RawValue::from_string(self.0.to_string())
                .map_err(serde::ser::Error::custom)?;
            raw.serialize(s)
        } else {
            s.serialize_str(&self.0)
        }
    }
}

impl<'de> Deserialize<'de> for JsonText {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            let raw: Box<serde_json::value::RawValue> = Deserialize::deserialize(d)?;
            Ok(Self(raw.get().into()))
        } else {
            let text = String::deserialize(d)?;
            Ok(Self(text.into()))
        }
    }
}

pub mod json_in_binary {
    use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            v.serialize(s)
        } else {
            let text = serde_json::to_string(v).map_err(serde::ser::Error::custom)?;
            s.serialize_str(&text)
        }
    }

    pub fn deserialize<'de, T: DeserializeOwned, D: Deserializer<'de>>(
        d: D,
    ) -> Result<T, D::Error> {
        if d.is_human_readable() {
            T::deserialize(d)
        } else {
            let text = String::deserialize(d)?;
            serde_json::from_str(&text).map_err(serde::de::Error::custom)
        }
    }
}

/// World positions on the wire: human-readable encodings carry the `f64` as is; the binary
/// encoding quantises to whole centimetres (`i32`, postcard varint: 1–3 bytes instead of 8).
/// Plan 4.7 — "position to 1 cm".
pub mod pos_cm {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_f64(*v)
        } else {
            let cm = (v * 100.0).round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
            s.serialize_i32(cm)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        if d.is_human_readable() {
            f64::deserialize(d)
        } else {
            Ok(i32::deserialize(d)? as f64 / 100.0)
        }
    }
}

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

/// Binary frames once `transport {compress: "lz4"}` was negotiated: a 1-byte tag, `0` = raw
/// payload, `1` = `lz4_flex` block with the uncompressed size prepended. Payloads up to
/// `LZ4_MIN_BYTES` are never compressed (the tag alone costs a byte, the block header four).
pub const LZ4_MIN_BYTES: usize = 256;

pub fn lz4_wrap(bytes: Bytes) -> Bytes {
    let mut out = Vec::with_capacity(bytes.len() + 5);
    if bytes.len() > LZ4_MIN_BYTES {
        out.push(1);
        out.extend_from_slice(&lz4_flex::compress_prepend_size(&bytes));
        if out.len() < bytes.len() + 1 {
            return Bytes::from(out);
        }
        out.clear();
    }
    out.push(0);
    out.extend_from_slice(&bytes);
    Bytes::from(out)
}

pub fn lz4_unwrap(b: &[u8], max_bytes: usize) -> Result<Bytes, ProtoError> {
    match b.first() {
        Some(0) => Ok(Bytes::copy_from_slice(&b[1..])),
        Some(1) => {
            let body = &b[1..];
            if body.len() < 4 {
                return Err(ProtoError("lz4 frame too short".into()));
            }
            let size = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
            if size > max_bytes {
                return Err(ProtoError(format!("lz4 frame expands to {size} bytes")));
            }
            lz4_flex::decompress_size_prepended(body)
                .map(Bytes::from)
                .map_err(|e| ProtoError(format!("lz4: {e}")))
        }
        Some(t) => Err(ProtoError(format!("unknown compression tag {t}"))),
        None => Err(ProtoError("empty frame".into())),
    }
}

/// A connected user as seen by other users.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct UserPresence {
    pub account_id: AccountId,
    pub session_id: SessionId,
    pub username: String,
    #[serde(default)]
    pub status: Option<String>,
    /// Structured rich presence (activity, party size, joinable …): a JSON object ≤ 2 KB,
    /// shown to followers next to `status`.
    #[serde(default, with = "crate::json_in_binary")]
    pub rich: Option<serde_json::Value>,
    /// Appear offline: followers see a leave and the session is left out of presence
    /// snapshots; the server still knows it is online.
    #[serde(default)]
    pub invisible: bool,
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
                rich: None,
                appear_offline: None,
            },
        };
        for enc in [Encoding::Json, Encoding::Postcard] {
            let b = enc.encode(&env).unwrap();
            let back: Envelope<ClientMessage> = enc.decode(&b).unwrap();
            assert_eq!(back, env);
        }
    }

    /// Postcard is not self-describing: a field skipped on encode cannot be defaulted on
    /// decode, so optional fields must always be written (world subscribe used to skip them).
    #[test]
    fn postcard_roundtrip_with_absent_optionals() {
        let env = Envelope {
            cid: None,
            msg: ClientMessage::WorldSubscribe {
                world: "w".into(),
                x: 1.0,
                y: 2.0,
                radius: 50.0,
                budget_bytes: None,
                filter: None,
                max_hz: None,
                unreliable_updates: false,
            },
        };
        let b = Encoding::Postcard.encode(&env).unwrap();
        let back: Envelope<ClientMessage> = Encoding::Postcard.decode(&b).unwrap();
        assert_eq!(back, env);
    }

    /// `serde_json::Value` fields ride the binary wire as JSON text (postcard has no
    /// `deserialize_any`), and stay plain JSON in the JSON encoding.
    #[test]
    fn free_form_json_roundtrips_in_both_encodings() {
        let env = Envelope {
            cid: None,
            msg: ClientMessage::WorldSubscribe {
                world: "w".into(),
                x: 0.0,
                y: 0.0,
                radius: 10.0,
                budget_bytes: Some(1024),
                filter: Some(serde_json::json!({"team": "red", "tags": [1, 2.5, null]})),
                max_hz: None,
                unreliable_updates: false,
            },
        };
        for enc in [Encoding::Json, Encoding::Postcard] {
            let b = enc.encode(&env).unwrap();
            let back: Envelope<ClientMessage> = enc.decode(&b).unwrap();
            assert_eq!(back, env);
        }
        let text = String::from_utf8(Encoding::Json.encode(&env).unwrap().to_vec()).unwrap();
        assert!(text.contains(r#""team":"red""#), "{text}");
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

#[cfg(test)]
mod json_text_tests {
    use super::*;

    #[test]
    fn json_text_round_trips_in_both_encodings_and_is_raw_in_json() {
        let v = serde_json::json!({"hp": 3, "tags": ["a", "b"], "nested": {"x": 1.5}});
        let c = realtime::WorldComponent {
            name: "stats".into(),
            value: JsonText::from_value(&v),
            version: 7,
        };
        let json = Encoding::Json.encode(&c).unwrap();
        let as_value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(
            as_value["value"], v,
            "human-readable form carries the object itself"
        );
        let back: realtime::WorldComponent = Encoding::Json.decode(&json).unwrap();
        assert_eq!(back.value.to_value(), v);
        let bin = Encoding::Postcard.encode(&c).unwrap();
        let back: realtime::WorldComponent = Encoding::Postcard.decode(&bin).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.value.to_value(), v);
    }
}

#[cfg(test)]
mod pos_cm_tests {
    use crate::realtime::{ServerMessage, WorldEntity};

    fn delta(x: f64, y: f64) -> ServerMessage {
        ServerMessage::WorldDelta {
            world: "w".into(),
            tick: 7,
            spawned: vec![],
            updated: vec![WorldEntity {
                id: uuid::Uuid::nil(),
                x,
                y,
                components: vec![],
            }],
            despawned: vec![uuid::Uuid::nil()],
            time_scale: 1.0,
            sequenced: true,
        }
    }

    fn pos(m: &ServerMessage) -> (f64, f64) {
        match m {
            ServerMessage::WorldDelta { updated, .. } => (updated[0].x, updated[0].y),
            _ => unreachable!(),
        }
    }

    #[test]
    fn json_keeps_full_precision() {
        let m = delta(12.3456, -0.001);
        let back: ServerMessage =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(pos(&back), (12.3456, -0.001));
    }

    #[test]
    fn postcard_rounds_to_centimetres_and_is_smaller() {
        let m = delta(12.3456, -1234567.891);
        let bytes = postcard::to_allocvec(&m).unwrap();
        let back: ServerMessage = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(pos(&back), (12.35, -1234567.89));
        // two f64 would be 16 bytes; two centimetre varints of this size are at most 8
        let f64_len = postcard::to_allocvec(&(12.3456f64, -1234567.891f64))
            .unwrap()
            .len();
        let cm_len = postcard::to_allocvec(&(1235i32, -123456789i32))
            .unwrap()
            .len();
        assert!(cm_len < f64_len, "{cm_len} vs {f64_len}");
    }
}
