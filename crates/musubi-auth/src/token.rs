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

/// Signs and verifies access tokens. HS256 with the shared secret in dev; ES256 when a
/// PEM key pair is configured.
#[derive(Clone)]
pub struct TokenSigner {
    enc: EncodingKey,
    dec: DecodingKey,
    alg: Algorithm,
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
            enc,
            dec,
            alg,
            access_ttl_secs: cfg.access_ttl_secs,
        })
    }

    pub fn sign(&self, claims: &Claims) -> Result<String> {
        jsonwebtoken::encode(&Header::new(self.alg), claims, &self.enc).map_err(Error::internal)
    }

    pub fn verify(&self, token: &str) -> Result<Claims> {
        let mut v = Validation::new(self.alg);
        v.leeway = 5;
        jsonwebtoken::decode::<Claims>(token, &self.dec, &v)
            .map(|d| d.claims)
            .map_err(|e| Error::Unauthenticated(format!("invalid token: {e}")))
    }
}
