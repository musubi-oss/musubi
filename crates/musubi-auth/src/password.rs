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

pub async fn verify(password: String, hash: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
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
