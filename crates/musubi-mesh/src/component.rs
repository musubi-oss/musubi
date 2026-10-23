//! A component's value: JSON for everything hand-written, raw bytes for engine blobs (the GDK's
//! `iris`/`iris.d` state). Bytes are kept as bytes in memory and on binary wires (CBOR sim stream,
//! `Accept: application/cbor` REST); every JSON surface — snapshots in Postgres, the JSON sim
//! stream, client frames — sees them as the same base64 string as before, so nothing stored or
//! speaking JSON changes shape.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Debug, Clone)]
pub enum ComponentValue {
    Json(Value),
    Bytes(Arc<[u8]>),
}

impl ComponentValue {
    pub fn bytes(b: impl Into<Arc<[u8]>>) -> Self {
        Self::Bytes(b.into())
    }
    /// The JSON form (`None` for bytes — use [`Self::to_json`] when a `Value` is really needed).
    pub fn as_json(&self) -> Option<&Value> {
        match self {
            Self::Json(v) => Some(v),
            Self::Bytes(_) => None,
        }
    }
    /// The bytes form: raw bytes, or a JSON string that decodes as standard base64.
    pub fn as_bytes(&self) -> Option<std::borrow::Cow<'_, [u8]>> {
        match self {
            Self::Bytes(b) => Some(std::borrow::Cow::Borrowed(b)),
            Self::Json(Value::String(s)) => B64.decode(s).ok().map(std::borrow::Cow::Owned),
            Self::Json(_) => None,
        }
    }
    /// What JSON surfaces see: bytes as a base64 string.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Json(v) => v.clone(),
            Self::Bytes(b) => Value::String(B64.encode(b)),
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Json(Value::Null))
    }
    /// `v.get(key)` on the JSON form.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_json()?.get(key)
    }
    /// Turn a JSON string that carries a blob of a bytes-typed component into bytes once, at the
    /// edge, so the hot path never re-decodes it.
    pub fn into_bytes_if_base64(self) -> Self {
        match self {
            Self::Json(Value::String(ref s)) => match B64.decode(s) {
                Ok(b) => Self::Bytes(b.into()),
                Err(_) => self,
            },
            other => other,
        }
    }
    pub fn to_json_text(&self) -> String {
        match self {
            Self::Json(v) => v.to_string(),
            Self::Bytes(b) => Value::String(B64.encode(b)).to_string(),
        }
    }
}

impl From<Value> for ComponentValue {
    fn from(v: Value) -> Self {
        Self::Json(v)
    }
}
impl From<Vec<u8>> for ComponentValue {
    fn from(b: Vec<u8>) -> Self {
        Self::Bytes(b.into())
    }
}

impl PartialEq for ComponentValue {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Self::Json(a), Self::Json(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            // a blob that arrived as base64 text equals the same blob that arrived as bytes
            (Self::Bytes(a), Self::Json(Value::String(s)))
            | (Self::Json(Value::String(s)), Self::Bytes(a)) => {
                B64.decode(s).is_ok_and(|d| d.as_slice() == &a[..])
            }
            _ => false,
        }
    }
}
impl PartialEq<Value> for ComponentValue {
    fn eq(&self, o: &Value) -> bool {
        match self {
            Self::Json(a) => a == o,
            Self::Bytes(a) => {
                matches!(o, Value::String(s) if B64.decode(s).is_ok_and(|d| d.as_slice() == &a[..]))
            }
        }
    }
}

impl Serialize for ComponentValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Json(v) => v.serialize(s),
            Self::Bytes(b) if s.is_human_readable() => s.serialize_str(&B64.encode(b)),
            Self::Bytes(b) => s.serialize_bytes(b),
        }
    }
}

impl<'de> Deserialize<'de> for ComponentValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ComponentValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value or a byte string")
            }
            fn visit_bytes<E: serde::de::Error>(self, b: &[u8]) -> Result<Self::Value, E> {
                Ok(ComponentValue::Bytes(b.into()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, b: Vec<u8>) -> Result<Self::Value, E> {
                Ok(ComponentValue::Bytes(b.into()))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(Value::Bool(v).into())
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Value::from(v).into())
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Value::from(v).into())
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(Value::from(v).into())
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Value::String(v.to_owned()).into())
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(Value::String(v).into())
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(Value::Null.into())
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(Value::Null.into())
            }
            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                d: D,
            ) -> Result<Self::Value, D::Error> {
                ComponentValue::deserialize(d)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> Result<Self::Value, A::Error> {
                Value::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))
                    .map(Into::into)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                Value::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(Into::into)
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bytes_are_base64_in_json_and_bytes_in_cbor() {
        let v = ComponentValue::bytes(vec![1u8, 2, 3, 250, 0, 255]);
        assert_eq!(serde_json::to_value(&v).unwrap(), json!("AQID+gD/"));
        let mut cbor = Vec::new();
        ciborium::into_writer(&v, &mut cbor).unwrap();
        assert_eq!(cbor[0], 0x46, "CBOR byte string of 6");
        let back: ComponentValue = ciborium::from_reader(cbor.as_slice()).unwrap();
        assert_eq!(back, v);
        let from_json: ComponentValue = serde_json::from_value(json!("AQID+gD/")).unwrap();
        assert!(
            from_json.as_json().is_some(),
            "JSON text stays JSON until the edge decodes it"
        );
        assert_eq!(from_json, v, "same blob, either form");
        assert_eq!(from_json.into_bytes_if_base64(), v);
        let hp: ComponentValue = ciborium::from_reader(&[0x07u8][..]).unwrap();
        assert_eq!(hp, json!(7));
        let obj: ComponentValue = serde_json::from_value(json!({"a": [1, "x"]})).unwrap();
        assert_eq!(obj.get("a"), Some(&json!([1, "x"])));
    }
}
