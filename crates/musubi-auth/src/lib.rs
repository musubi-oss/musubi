//! Identities, sessions and tokens.
//!
//! One account owns many identities (`device`, `email`, `steam`, …). An authentication
//! produces a *session*: a short-lived stateless JWT access token plus an opaque refresh
//! token stored hashed in `musubi.sessions`. Refresh tokens rotate; reuse of a rotated
//! token revokes the whole family.

mod password;
mod service;
pub mod token;

pub use service::{AuthService, Authenticated, Session};
pub use token::{Claims, TokenSigner};

use serde::{Deserialize, Serialize};

/// Built-in provider names. Custom providers register their own strings.
pub mod provider {
    pub const DEVICE: &str = "device";
    pub const EMAIL: &str = "email";
    pub const STEAM: &str = "steam";
    pub const GOOGLE: &str = "google";
    pub const APPLE: &str = "apple";
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
}

impl Default for AuthOptions {
    fn default() -> Self {
        Self {
            create: true,
            username: None,
            vars: Default::default(),
        }
    }
}

fn default_true() -> bool {
    true
}
