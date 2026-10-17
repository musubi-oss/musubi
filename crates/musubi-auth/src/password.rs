use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};
use musubi_core::{Error, Result};

fn hasher() -> Argon2<'static> {
    // argon2id, 64 MiB, t=3, p=1 — matches the design doc defaults.
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(64 * 1024, 3, 1, None).expect("valid argon2 params"),
    )
}

pub async fn hash(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        hasher()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(Error::internal)
    })
    .await
    .map_err(Error::internal)?
}

/// Verifies against our argon2id hashes and, for accounts imported from Nakama (`musubi migrate
/// nakama`), their bcrypt hashes (`$2a$`/`$2b$`/`$2y$`), so migrated players keep their passwords.
pub async fn verify(password: String, hash: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        if hash.starts_with("$2") {
            return Ok(bcrypt::verify(password.as_bytes(), &hash).unwrap_or(false));
        }
        let parsed = PasswordHash::new(&hash).map_err(Error::internal)?;
        Ok(hasher()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    })
    .await
    .map_err(Error::internal)?
}

pub fn validate(password: &str) -> Result<()> {
    if password.chars().count() < 8 {
        return Err(Error::InvalidArgument(
            "password must be at least 8 characters".into(),
        ));
    }
    if password.len() > 256 {
        return Err(Error::InvalidArgument("password too long".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn verifies_argon2_and_nakama_bcrypt() {
        let h = super::hash("correct horse".into()).await.unwrap();
        assert!(super::verify("correct horse".into(), h.clone())
            .await
            .unwrap());
        assert!(!super::verify("wrong".into(), h).await.unwrap());
        let b = bcrypt::hash("nakama-pass", 4).unwrap();
        assert!(super::verify("nakama-pass".into(), b.clone())
            .await
            .unwrap());
        assert!(!super::verify("other".into(), b).await.unwrap());
        assert!(!super::verify("x".into(), "$2b$garbage".into())
            .await
            .unwrap());
    }
}
