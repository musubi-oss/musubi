use std::sync::{Arc, RwLock};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use musubi_core::config::SessionConfig;
use musubi_core::{AccountId, Error, Result, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: AccountId,
    pub sid: SessionId,
    pub iat: i64,
    pub exp: i64,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub vars: serde_json::Map<String, serde_json::Value>,
}

/// A managed ES256 key (`musubi.jwt_keys`): PKCS#8 private key plus its public point for JWKS.
pub struct JwtKey {
    pub kid: String,
    pub pkcs8: Vec<u8>,
    pub retired: bool,
}

struct Managed {
    kid: String,
    enc: EncodingKey,
    dec: DecodingKey,
    x: String,
    y: String,
    retired: bool,
}

/// Signs and verifies access tokens. Without managed keys: HS256 with the shared secret (dev) or
/// ES256 with the configured PEM. Managed keys (rotated through the admin API) take over signing
/// with a `kid` header; every installed key still verifies until it is pruned, so rotation does
/// not log anyone out. The public halves are published at `/.well-known/jwks.json`.
#[derive(Clone)]
pub struct TokenSigner {
    legacy_enc: EncodingKey,
    legacy_dec: DecodingKey,
    legacy_alg: Algorithm,
    managed: Arc<RwLock<Vec<Managed>>>,
    pub access_ttl_secs: u64,
}

impl TokenSigner {
    pub fn from_config(cfg: &SessionConfig) -> Result<Self> {
        let (enc, dec, alg) = match &cfg.signing_key_pem {
            Some(pem) => {
                let enc = EncodingKey::from_ec_pem(pem.as_bytes()).map_err(Error::internal)?;
                let dec = DecodingKey::from_ec_pem(pem.as_bytes()).map_err(Error::internal)?;
                (enc, dec, Algorithm::ES256)
            }
            None => (
                EncodingKey::from_secret(cfg.secret.as_bytes()),
                DecodingKey::from_secret(cfg.secret.as_bytes()),
                Algorithm::HS256,
            ),
        };
        Ok(Self {
            legacy_enc: enc,
            legacy_dec: dec,
            legacy_alg: alg,
            managed: Arc::new(RwLock::new(Vec::new())),
            access_ttl_secs: cfg.access_ttl_secs,
        })
    }

    /// Generate a fresh P-256 key pair; returns (kid, PKCS#8 DER).
    pub fn generate_key() -> Result<(String, Vec<u8>)> {
        let rng = ring::rand::SystemRandom::new();
        let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .map_err(|_| Error::Internal("key generation failed".into()))?;
        let kid = uuid::Uuid::now_v7().simple().to_string()[..16].to_string();
        Ok((kid, doc.as_ref().to_vec()))
    }

    /// Replace the managed key set; `keys` newest first (the first non-retired key signs).
    pub fn install(&self, keys: Vec<JwtKey>) -> Result<()> {
        let rng = ring::rand::SystemRandom::new();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            let pair = ring::signature::EcdsaKeyPair::from_pkcs8(
                &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                &k.pkcs8,
                &rng,
            )
            .map_err(|_| {
                Error::Internal(format!("jwt key {} is not a valid P-256 PKCS#8 key", k.kid))
            })?;
            let point = ring::signature::KeyPair::public_key(&pair)
                .as_ref()
                .to_vec();
            if point.len() != 65 || point[0] != 4 {
                return Err(Error::Internal("unexpected EC public key encoding".into()));
            }
            let x = URL_SAFE_NO_PAD.encode(&point[1..33]);
            let y = URL_SAFE_NO_PAD.encode(&point[33..65]);
            out.push(Managed {
                dec: DecodingKey::from_ec_components(&x, &y).map_err(Error::internal)?,
                enc: EncodingKey::from_ec_der(&k.pkcs8),
                kid: k.kid,
                x,
                y,
                retired: k.retired,
            });
        }
        *self.managed.write().unwrap() = out;
        Ok(())
    }

    /// `kid` currently used for signing (None = legacy config key).
    pub fn active_kid(&self) -> Option<String> {
        self.managed
            .read()
            .unwrap()
            .iter()
            .find(|k| !k.retired)
            .map(|k| k.kid.clone())
    }

    /// RFC 7517 key set of every managed key (retired ones included while their tokens may live).
    pub fn jwks(&self) -> serde_json::Value {
        let keys: Vec<serde_json::Value> = self
            .managed
            .read()
            .unwrap()
            .iter()
            .map(|k| {
                serde_json::json!({ "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": k.kid, "x": k.x, "y": k.y })
            })
            .collect();
        serde_json::json!({ "keys": keys })
    }

    pub fn sign(&self, claims: &Claims) -> Result<String> {
        let managed = self.managed.read().unwrap();
        if let Some(k) = managed.iter().find(|k| !k.retired) {
            let mut h = Header::new(Algorithm::ES256);
            h.kid = Some(k.kid.clone());
            return jsonwebtoken::encode(&h, claims, &k.enc).map_err(Error::internal);
        }
        jsonwebtoken::encode(&Header::new(self.legacy_alg), claims, &self.legacy_enc)
            .map_err(Error::internal)
    }

    pub fn verify(&self, token: &str) -> Result<Claims> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|e| Error::Unauthenticated(format!("invalid token: {e}")))?;
        let (dec, alg) = match header.kid {
            Some(kid) => {
                let managed = self.managed.read().unwrap();
                let k = managed
                    .iter()
                    .find(|k| k.kid == kid)
                    .ok_or_else(|| Error::Unauthenticated("invalid token: unknown kid".into()))?;
                (k.dec.clone(), Algorithm::ES256)
            }
            None => (self.legacy_dec.clone(), self.legacy_alg),
        };
        let mut v = Validation::new(alg);
        v.leeway = 5;
        jsonwebtoken::decode::<Claims>(token, &dec, &v)
            .map(|d| d.claims)
            .map_err(|e| Error::Unauthenticated(format!("invalid token: {e}")))
    }
}
