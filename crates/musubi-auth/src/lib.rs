//! Identities, sessions and tokens.
//!
//! One account owns many identities (`device`, `email`, `steam`, …). An authentication
//! produces a *session*: a short-lived stateless JWT access token plus an opaque refresh
//! token stored hashed in `musubi.sessions`. Refresh tokens rotate; reuse of a rotated
//! token revokes the whole family.

pub mod passkey;
pub mod password;
mod service;
pub mod token;
pub mod totp;

pub use service::{
    AuthService, Authenticated, CaptchaFuture, CaptchaVerifier, CaptchaVerifyFn, Login, LoginQueue,
    QueueStats, Session, SessionInfo, MFA_TOKEN_SECS, QUEUE_TICKET_SECS,
};
pub use token::{Claims, TokenSigner};

use serde::{Deserialize, Serialize};

/// Built-in provider names. Custom providers register their own strings.
pub mod provider {
    pub const DEVICE: &str = "device";
    pub const EMAIL: &str = "email";
    pub const TOTP: &str = "totp";
    pub const STEAM: &str = "steam";
    pub const GOOGLE: &str = "google";
    pub const APPLE: &str = "apple";
    /// WebAuthn credential; `subject` is the base64url credential id.
    pub const PASSKEY: &str = "passkey";
    pub const CUSTOM: &str = "custom";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthOptions {
    /// Create the account if the identity is unknown (default true, like Nakama `create`).
    #[serde(default = "default_true")]
    pub create: bool,
    /// Username for a newly created account; generated when absent.
    #[serde(default)]
    pub username: Option<String>,
    /// Arbitrary session variables embedded in the access token (≤ 4 KB).
    #[serde(default)]
    pub vars: serde_json::Map<String, serde_json::Value>,
    /// Captcha response token, required once an address has `captcha_after` failed logins.
    #[serde(default)]
    pub captcha: Option<String>,
}

impl Default for AuthOptions {
    fn default() -> Self {
        Self {
            create: true,
            username: None,
            vars: Default::default(),
            captcha: None,
        }
    }
}

fn default_true() -> bool {
    true
}
