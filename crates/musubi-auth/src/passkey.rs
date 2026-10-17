//! Passkeys (WebAuthn level 2, attestation `none`) on `ring`: parse the authenticator's
//! `attestationObject` at registration, keep the COSE public key, verify assertions at login.
//! Only what a relying party needs — no attestation-chain checks, no extensions.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use musubi_core::{Error, Result};
use ring::signature::{self, UnparsedPublicKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const ES256: i64 = -7;
pub const RS256: i64 = -257;

/// Minimal CBOR (RFC 8949) value, enough for attestation objects and COSE keys.
#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Uint(u64),
    Neg(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Other,
}

impl Cbor {
    pub fn get(&self, key: &Cbor) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn get_text(&self, key: &str) -> Option<&Cbor> {
        self.get(&Cbor::Text(key.into()))
    }
    fn get_int(&self, key: i64) -> Option<&Cbor> {
        let k = if key >= 0 {
            Cbor::Uint(key as u64)
        } else {
            Cbor::Neg(key)
        };
        self.get(&k)
    }
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }
    fn int(&self) -> Option<i64> {
        match self {
            Cbor::Uint(u) => i64::try_from(*u).ok(),
            Cbor::Neg(n) => Some(*n),
            _ => None,
        }
    }
}

/// Decode one CBOR item; `(value, bytes consumed)`.
pub fn decode_cbor(b: &[u8]) -> Result<(Cbor, usize)> {
    fn bad() -> Error {
        Error::InvalidArgument("malformed cbor".into())
    }
    let first = *b.first().ok_or_else(bad)?;
    let (major, info) = (first >> 5, first & 0x1f);
    let (arg, mut pos): (u64, usize) = match info {
        0..=23 => (info as u64, 1),
        24 => (*b.get(1).ok_or_else(bad)? as u64, 2),
        25 => (
            u16::from_be_bytes(b.get(1..3).ok_or_else(bad)?.try_into().unwrap()) as u64,
            3,
        ),
        26 => (
            u32::from_be_bytes(b.get(1..5).ok_or_else(bad)?.try_into().unwrap()) as u64,
            5,
        ),
        27 => (
            u64::from_be_bytes(b.get(1..9).ok_or_else(bad)?.try_into().unwrap()),
            9,
        ),
        _ => return Err(bad()),
    };
    let len = usize::try_from(arg).map_err(|_| bad())?;
    let v = match major {
        0 => Cbor::Uint(arg),
        1 => Cbor::Neg(-1 - i64::try_from(arg).map_err(|_| bad())?),
        2 | 3 => {
            let s = b.get(pos..pos + len).ok_or_else(bad)?;
            pos += len;
            if major == 2 {
                Cbor::Bytes(s.to_vec())
            } else {
                Cbor::Text(String::from_utf8(s.to_vec()).map_err(|_| bad())?)
            }
        }
        4 => {
            let mut items = Vec::with_capacity(len.min(64));
            for _ in 0..len {
                let (v, n) = decode_cbor(&b[pos..])?;
                pos += n;
                items.push(v);
            }
            Cbor::Array(items)
        }
        5 => {
            let mut items = Vec::with_capacity(len.min(64));
            for _ in 0..len {
                let (k, n) = decode_cbor(&b[pos..])?;
                pos += n;
                let (v, n) = decode_cbor(&b[pos..])?;
                pos += n;
                items.push((k, v));
            }
            Cbor::Map(items)
        }
        6 => {
            let (_, n) = decode_cbor(&b[pos..])?;
            pos += n;
            Cbor::Other
        }
        _ => Cbor::Other,
    };
    Ok((v, pos))
}

/// A registered credential's public key, as stored.
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub struct PublicKey {
    pub alg: i64,
    /// ES256: uncompressed SEC1 point (65 bytes). RS256: modulus (big-endian).
    pub key: Vec<u8>,
    /// RS256 only: public exponent (big-endian).
    #[serde(default)]
    pub exponent: Vec<u8>,
}

impl PublicKey {
    fn from_cose(cose: &Cbor) -> Result<Self> {
        let bad = |m: &str| Error::InvalidArgument(format!("cose key: {m}"));
        let kty = cose
            .get_int(1)
            .and_then(Cbor::int)
            .ok_or_else(|| bad("kty"))?;
        let alg = cose
            .get_int(3)
            .and_then(Cbor::int)
            .ok_or_else(|| bad("alg"))?;
        match (kty, alg) {
            (2, ES256) => {
                if cose.get_int(-1).and_then(Cbor::int) != Some(1) {
                    return Err(bad("curve must be P-256"));
                }
                let x = cose
                    .get_int(-2)
                    .and_then(Cbor::bytes)
                    .ok_or_else(|| bad("x"))?;
                let y = cose
                    .get_int(-3)
                    .and_then(Cbor::bytes)
                    .ok_or_else(|| bad("y"))?;
                if x.len() != 32 || y.len() != 32 {
                    return Err(bad("point size"));
                }
                let mut key = Vec::with_capacity(65);
                key.push(4);
                key.extend_from_slice(x);
                key.extend_from_slice(y);
                Ok(Self {
                    alg,
                    key,
                    exponent: vec![],
                })
            }
            (3, RS256) => {
                let n = cose
                    .get_int(-1)
                    .and_then(Cbor::bytes)
                    .ok_or_else(|| bad("n"))?;
                let e = cose
                    .get_int(-2)
                    .and_then(Cbor::bytes)
                    .ok_or_else(|| bad("e"))?;
                if n.len() < 256 {
                    return Err(bad("rsa modulus under 2048 bits"));
                }
                Ok(Self {
                    alg,
                    key: n.to_vec(),
                    exponent: e.to_vec(),
                })
            }
            _ => Err(bad("unsupported algorithm (ES256 or RS256)")),
        }
    }

    fn verify(&self, msg: &[u8], sig: &[u8]) -> Result<()> {
        let fail = || Error::Unauthenticated("passkey signature invalid".into());
        match self.alg {
            ES256 => UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, &self.key)
                .verify(msg, sig)
                .map_err(|_| fail()),
            RS256 => signature::RsaPublicKeyComponents {
                n: &self.key,
                e: &self.exponent,
            }
            .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
            .map_err(|_| fail()),
            _ => Err(fail()),
        }
    }
}

/// Parsed authenticator data (WebAuthn §6.1).
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    /// Present when the attested-credential-data flag is set (registration).
    pub credential: Option<(Vec<u8>, PublicKey)>,
}

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;

pub fn parse_auth_data(b: &[u8]) -> Result<AuthData> {
    let bad = |m: &str| Error::InvalidArgument(format!("authenticator data: {m}"));
    if b.len() < 37 {
        return Err(bad("too short"));
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&b[..32]);
    let flags = b[32];
    let sign_count = u32::from_be_bytes(b[33..37].try_into().unwrap());
    let credential = if flags & FLAG_AT != 0 {
        let rest = &b[37..];
        if rest.len() < 18 {
            return Err(bad("attested credential data too short"));
        }
        let id_len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        let id = rest
            .get(18..18 + id_len)
            .ok_or_else(|| bad("credential id"))?;
        if id.is_empty() || id_len > 1023 {
            return Err(bad("credential id length"));
        }
        let (cose, _) = decode_cbor(&rest[18 + id_len..])?;
        Some((id.to_vec(), PublicKey::from_cose(&cose)?))
    } else {
        None
    };
    Ok(AuthData {
        rp_id_hash,
        flags,
        sign_count,
        credential,
    })
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

fn check_client_data(
    client_data_json: &[u8],
    kind: &str,
    challenge: &[u8],
    origins: &[String],
) -> Result<()> {
    let c: ClientData = serde_json::from_slice(client_data_json)
        .map_err(|e| Error::InvalidArgument(format!("clientDataJSON: {e}")))?;
    if c.kind != kind {
        return Err(Error::Unauthenticated(format!(
            "clientDataJSON type {} (expected {kind})",
            c.kind
        )));
    }
    let got = URL_SAFE_NO_PAD
        .decode(c.challenge.as_bytes())
        .map_err(|_| Error::Unauthenticated("challenge not base64url".into()))?;
    if got != challenge {
        return Err(Error::Unauthenticated("challenge mismatch".into()));
    }
    if !origins
        .iter()
        .any(|o| o.trim_end_matches('/') == c.origin.trim_end_matches('/'))
    {
        return Err(Error::Unauthenticated(format!(
            "origin {} not allowed",
            c.origin
        )));
    }
    Ok(())
}

fn rp_hash_ok(rp_id: &str, got: &[u8; 32]) -> Result<()> {
    if Sha256::digest(rp_id.as_bytes()).as_slice() != got {
        return Err(Error::Unauthenticated("rpIdHash mismatch".into()));
    }
    Ok(())
}

/// `navigator.credentials.create()` result: `{ id, response: { clientDataJSON, attestationObject } }`.
#[derive(Deserialize)]
pub struct Attestation {
    pub id: String,
    pub response: AttestationResponse,
}

#[derive(Deserialize)]
pub struct AttestationResponse {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "attestationObject")]
    pub attestation_object: String,
}

pub struct Registered {
    pub credential_id: Vec<u8>,
    pub public_key: PublicKey,
    pub sign_count: u32,
    pub user_verified: bool,
}

/// Validate a registration (§7.1, attestation statement not verified) and return what to store.
pub fn register(
    a: &Attestation,
    challenge: &[u8],
    rp_id: &str,
    origins: &[String],
) -> Result<Registered> {
    let b64 = |s: &str| {
        URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .map_err(|_| Error::InvalidArgument("field is not base64url".into()))
    };
    let cdj = b64(&a.response.client_data_json)?;
    check_client_data(&cdj, "webauthn.create", challenge, origins)?;
    let att = b64(&a.response.attestation_object)?;
    let (obj, _) = decode_cbor(&att)?;
    let auth_data = obj
        .get_text("authData")
        .and_then(Cbor::bytes)
        .ok_or_else(|| Error::InvalidArgument("attestationObject without authData".into()))?;
    let ad = parse_auth_data(auth_data)?;
    rp_hash_ok(rp_id, &ad.rp_id_hash)?;
    if ad.flags & FLAG_UP == 0 {
        return Err(Error::Unauthenticated("user presence flag not set".into()));
    }
    let (credential_id, public_key) = ad
        .credential
        .ok_or_else(|| Error::InvalidArgument("no attested credential data".into()))?;
    if b64(&a.id)? != credential_id {
        return Err(Error::InvalidArgument(
            "id does not match credential".into(),
        ));
    }
    Ok(Registered {
        credential_id,
        public_key,
        sign_count: ad.sign_count,
        user_verified: ad.flags & FLAG_UV != 0,
    })
}

/// `navigator.credentials.get()` result.
#[derive(Deserialize)]
pub struct Assertion {
    pub id: String,
    pub response: AssertionResponse,
}

#[derive(Deserialize)]
pub struct AssertionResponse {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    pub signature: String,
}

/// Verify an assertion (§7.2) against a stored key; returns the new sign count.
/// A counter that did not advance past `stored_count` (when either is non-zero) means a cloned
/// authenticator and is refused.
pub fn verify_assertion(
    a: &Assertion,
    key: &PublicKey,
    stored_count: u32,
    challenge: &[u8],
    rp_id: &str,
    origins: &[String],
) -> Result<u32> {
    let b64 = |s: &str| {
        URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .map_err(|_| Error::InvalidArgument("field is not base64url".into()))
    };
    let cdj = b64(&a.response.client_data_json)?;
    check_client_data(&cdj, "webauthn.get", challenge, origins)?;
    let auth_data = b64(&a.response.authenticator_data)?;
    let ad = parse_auth_data(&auth_data)?;
    rp_hash_ok(rp_id, &ad.rp_id_hash)?;
    if ad.flags & FLAG_UP == 0 {
        return Err(Error::Unauthenticated("user presence flag not set".into()));
    }
    let mut msg = auth_data.clone();
    msg.extend_from_slice(&Sha256::digest(&cdj));
    key.verify(&msg, &b64(&a.response.signature)?)?;
    if (ad.sign_count != 0 || stored_count != 0) && ad.sign_count <= stored_count {
        return Err(Error::Unauthenticated(
            "authenticator counter did not advance (cloned key?)".into(),
        ));
    }
    Ok(ad.sign_count)
}

/// Test/SDK helper: build the CBOR `attestationObject` (fmt `none`) and authenticator data for a
/// P-256 public point, the way an authenticator would.
pub fn build_none_attestation(
    rp_id: &str,
    credential_id: &[u8],
    sec1_point: &[u8],
    sign_count: u32,
    flags: u8,
) -> Vec<u8> {
    let mut ad = Vec::new();
    ad.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
    ad.push(flags | FLAG_AT);
    ad.extend_from_slice(&sign_count.to_be_bytes());
    ad.extend_from_slice(&[0u8; 16]);
    ad.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
    ad.extend_from_slice(credential_id);
    // COSE_Key {1: 2, 3: -7, -1: 1, -2: x, -3: y}
    ad.extend_from_slice(&[0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20]);
    ad.extend_from_slice(&sec1_point[1..33]);
    ad.extend_from_slice(&[0x22, 0x58, 0x20]);
    ad.extend_from_slice(&sec1_point[33..65]);
    // {"fmt": "none", "attStmt": {}, "authData": bytes}
    let mut out = vec![0xa3, 0x63];
    out.extend_from_slice(b"fmt");
    out.push(0x64);
    out.extend_from_slice(b"none");
    out.push(0x67);
    out.extend_from_slice(b"attStmt");
    out.push(0xa0);
    out.push(0x68);
    out.extend_from_slice(b"authData");
    out.push(0x59);
    out.extend_from_slice(&(ad.len() as u16).to_be_bytes());
    out.extend_from_slice(&ad);
    out
}

/// Test/SDK helper: authenticator data for an assertion (no attested credential).
pub fn build_assertion_auth_data(rp_id: &str, sign_count: u32, flags: u8) -> Vec<u8> {
    let mut ad = Vec::new();
    ad.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
    ad.push(flags);
    ad.extend_from_slice(&sign_count.to_be_bytes());
    ad
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    fn client_data(kind: &str, challenge: &[u8], origin: &str) -> String {
        URL_SAFE_NO_PAD.encode(
            serde_json::json!({"type": kind, "challenge": URL_SAFE_NO_PAD.encode(challenge), "origin": origin})
                .to_string(),
        )
    }

    #[test]
    fn register_then_assert_es256() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let cred = b"cred-1".to_vec();
        let origins = vec!["https://play.example".to_string()];
        let challenge = [7u8; 32];
        let att = Attestation {
            id: URL_SAFE_NO_PAD.encode(&cred),
            response: AttestationResponse {
                client_data_json: client_data("webauthn.create", &challenge, &origins[0]),
                attestation_object: URL_SAFE_NO_PAD.encode(build_none_attestation(
                    "example",
                    &cred,
                    kp.public_key().as_ref(),
                    0,
                    FLAG_UP | FLAG_UV,
                )),
            },
        };
        let reg = register(&att, &challenge, "example", &origins).unwrap();
        assert_eq!(reg.credential_id, cred);
        assert_eq!(reg.public_key.alg, ES256);
        assert!(reg.user_verified);
        assert!(register(&att, &[8u8; 32], "example", &origins).is_err());
        assert!(register(&att, &challenge, "other", &origins).is_err());

        let ch2 = [9u8; 32];
        let cdj = client_data("webauthn.get", &ch2, &origins[0]);
        let ad = build_assertion_auth_data("example", 5, FLAG_UP);
        let mut msg = ad.clone();
        msg.extend_from_slice(&Sha256::digest(URL_SAFE_NO_PAD.decode(&cdj).unwrap()));
        let sig = kp.sign(&rng, &msg).unwrap();
        let a = Assertion {
            id: att.id.clone(),
            response: AssertionResponse {
                client_data_json: cdj.clone(),
                authenticator_data: URL_SAFE_NO_PAD.encode(&ad),
                signature: URL_SAFE_NO_PAD.encode(sig.as_ref()),
            },
        };
        assert_eq!(
            verify_assertion(&a, &reg.public_key, 0, &ch2, "example", &origins).unwrap(),
            5
        );
        // counter must advance
        assert!(verify_assertion(&a, &reg.public_key, 5, &ch2, "example", &origins).is_err());
        // wrong origin / tampered signature
        assert!(verify_assertion(
            &a,
            &reg.public_key,
            0,
            &ch2,
            "example",
            &["https://evil".into()]
        )
        .is_err());
        let mut bad = a;
        bad.response.signature = URL_SAFE_NO_PAD.encode([1u8; 70]);
        assert!(verify_assertion(&bad, &reg.public_key, 0, &ch2, "example", &origins).is_err());
    }

    #[test]
    fn cbor_roundtrip_shapes() {
        let (v, n) = decode_cbor(&[0xa2, 0x01, 0x02, 0x20, 0x43, 1, 2, 3]).unwrap();
        assert_eq!(n, 8);
        assert_eq!(v.get_int(1), Some(&Cbor::Uint(2)));
        assert_eq!(v.get_int(-1), Some(&Cbor::Bytes(vec![1, 2, 3])));
        assert!(decode_cbor(&[0x59, 0xff, 0xff, 0x00]).is_err());
    }
}
