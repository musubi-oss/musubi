use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};
use musubi_core::{Error, Result};
use std::sync::LazyLock;

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

/// A real argon2id hash with a fixed password, so logins for unknown addresses or identities
/// without a password can burn the same work as a wrong password (no timing enumeration).
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    let salt = SaltString::generate(&mut OsRng);
    hasher()
        .hash_password(b"musubi-dummy-password", &salt)
        .map(|h| h.to_string())
        .expect("argon2 hash of a constant")
});

/// Spend the cost of one password verification without an account to verify against; the
/// result is discarded. Call it on the "no such user" branch of a login.
pub async fn verify_dummy(password: String) {
    let _ = verify(password, DUMMY_HASH.clone()).await;
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

/// Operator-tunable password rules (`[session] password_*`); `Default` is the historical
/// behaviour: 8–256 characters, common passwords refused.
#[derive(Debug, Clone)]
pub struct Policy {
    pub min_length: usize,
    pub require_classes: u8,
    pub deny_common: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min_length: 8,
            require_classes: 0,
            deny_common: true,
        }
    }
}

const COMMON: &[&str] = &[
    "password",
    "password1",
    "password123",
    "passw0rd",
    "p@ssw0rd",
    "12345678",
    "123456789",
    "1234567890",
    "qwerty123",
    "qwertyuiop",
    "1q2w3e4r",
    "1q2w3e4r5t",
    "iloveyou",
    "sunshine",
    "princess",
    "football",
    "baseball",
    "superman",
    "trustno1",
    "welcome1",
    "letmein1",
    "abc12345",
    "monkey123",
    "dragon123",
    "master123",
    "shadow123",
    "michael1",
    "jennifer",
    "computer",
    "internet",
    "whatever",
    "starwars",
    "pokemon1",
    "minecraft",
    "fortnite",
    "asdfghjkl",
    "zxcvbnm1",
    "11111111",
    "00000000",
    "88888888",
    "qazwsxedc",
    "admin123",
    "changeme",
    "password!",
    "pass1234",
    "abcd1234",
    "a1b2c3d4",
    "qwer1234",
];

impl Policy {
    pub fn check(&self, password: &str, email: Option<&str>) -> Result<()> {
        let n = password.chars().count();
        if n < self.min_length.max(1) {
            return Err(Error::InvalidArgument(format!(
                "password must be at least {} characters",
                self.min_length.max(1)
            )));
        }
        if password.len() > 256 {
            return Err(Error::InvalidArgument("password too long".into()));
        }
        if self.require_classes > 0 {
            let lower = password.chars().any(|c| c.is_lowercase());
            let upper = password.chars().any(|c| c.is_uppercase());
            let digit = password.chars().any(|c| c.is_ascii_digit());
            let other = password
                .chars()
                .any(|c| !c.is_alphanumeric() && !c.is_whitespace());
            let classes = [lower, upper, digit, other].iter().filter(|b| **b).count() as u8;
            if classes < self.require_classes.min(4) {
                return Err(Error::InvalidArgument(format!(
                    "password must mix at least {} of: lowercase, uppercase, digits, symbols",
                    self.require_classes.min(4)
                )));
            }
        }
        if self.deny_common {
            let lower = password.to_lowercase();
            let stripped: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
            if COMMON.contains(&stripped.as_str()) {
                return Err(Error::InvalidArgument("password is too common".into()));
            }
            let mut chars = stripped.chars();
            if let Some(first) = chars.next() {
                if chars.all(|c| c == first) {
                    return Err(Error::InvalidArgument(
                        "password is one repeated character".into(),
                    ));
                }
            }
            if is_sequence(&stripped) {
                return Err(Error::InvalidArgument(
                    "password is a simple sequence".into(),
                ));
            }
            if let Some(local) = email.and_then(|e| e.split('@').next()) {
                let local = local.to_lowercase();
                if local.chars().count() >= 4 && stripped.contains(&local) {
                    return Err(Error::InvalidArgument(
                        "password must not contain your email".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// `abcdefgh`, `12345678`, `87654321`, `qwertyuiop12`: a run of neighbours one step apart in
/// the alphabet, digits, or along a keyboard row that makes up most of the password.
fn is_sequence(s: &str) -> bool {
    const ROWS: &[&str] = &["qwertyuiop", "asdfghjkl", "zxcvbnm"];
    let v: Vec<char> = s.chars().collect();
    if v.len() < 4 {
        return false;
    }
    let step = |a: char, b: char| -> bool {
        if a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric() {
            let (a8, b8) = (a as i32, b as i32);
            if (a8 - b8).abs() == 1 && a.is_ascii_digit() == b.is_ascii_digit() {
                return true;
            }
        }
        ROWS.iter().any(|r| {
            let (Some(i), Some(j)) = (r.find(a), r.find(b)) else {
                return false;
            };
            (i as i32 - j as i32).abs() == 1
        })
    };
    let (mut longest, mut run) = (1usize, 1usize);
    for w in v.windows(2) {
        run = if step(w[0], w[1]) { run + 1 } else { 1 };
        longest = longest.max(run);
    }
    longest == v.len() || (longest >= 6 && longest * 3 >= v.len() * 2)
}

/// Length-only sanity for places without a policy (login attempts: the stored hash decides).
pub fn validate(password: &str) -> Result<()> {
    Policy {
        min_length: 1,
        require_classes: 0,
        deny_common: false,
    }
    .check(password, None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn policy_rejects_the_usual_suspects() {
        let p = super::Policy::default();
        assert!(p.check("correct horse battery", None).is_ok());
        assert!(p.check("short", None).is_err());
        assert!(p.check("Password123", None).is_err());
        assert!(p.check("aaaaaaaa", None).is_err());
        assert!(p.check("12345678", None).is_err());
        assert!(p.check("qwertyuiop", None).is_err());
        assert!(p.check("qwertyuiop12", None).is_err());
        assert!(p.check("abcdefgh9", None).is_err());
        assert!(p.check("abcde-horse-9", None).is_ok());
        assert!(p
            .check("johnsmith99", Some("JohnSmith@example.test"))
            .is_err());
        assert!(p.check("johnsmith99", Some("jo@example.test")).is_ok());
        let strict = super::Policy {
            min_length: 12,
            require_classes: 3,
            deny_common: true,
        };
        assert!(strict.check("correct horse battery", None).is_err());
        assert!(strict.check("Correct-horse-battery", None).is_ok());
        assert!(strict.check("Correct horse 9", None).is_ok());
    }

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
