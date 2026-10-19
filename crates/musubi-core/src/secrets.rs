//! P1-30 secrets management.
//!
//! * Config indirection: any string value may contain `${env:NAME}` or `${file:/path}`; both are
//!   resolved once at load (missing → load error naming the config path), so secrets live in the
//!   orchestrator / a mounted volume / a Vault-agent sink rather than in the TOML.
//! * Encryption at rest: credentials stored in Postgres (webhook signing secrets, IdP client
//!   secrets, …) are sealed with ChaCha20-Poly1305 under `[security] master_keys` — the first key
//!   seals, every key opens, so rotation = prepend a key, re-encrypt, drop the old one. Values are
//!   `enc:v1:<kid>:<base64(nonce ‖ ciphertext)>`; anything else is treated as plaintext, so
//!   enabling the feature on an existing deployment needs no migration.
use std::sync::{OnceLock, RwLock};

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "enc:v1:";

/// Expand `${env:…}` / `${file:…}` in every string of `v`; `out` collects the config paths that
/// contained a reference.
pub fn resolve_refs(
    v: &mut serde_json::Value,
    path: &str,
    out: &mut Vec<String>,
) -> Result<(), String> {
    match v {
        serde_json::Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                resolve_refs(child, &p, out)?;
            }
        }
        serde_json::Value::Array(a) => {
            for (i, child) in a.iter_mut().enumerate() {
                resolve_refs(child, &format!("{path}[{i}]"), out)?;
            }
        }
        serde_json::Value::String(s) if s.contains("${") => {
            *s = expand(s, path)?;
            out.push(path.to_string());
        }
        _ => {}
    }
    Ok(())
}

pub fn expand(s: &str, path: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("config {path}: unterminated ${{…}} reference"));
        };
        let inner = &after[..end];
        let (kind, name) = inner.split_once(':').ok_or_else(|| {
            format!("config {path}: ${{{inner}}} must be ${{env:NAME}} or ${{file:/path}}")
        })?;
        let value = match kind {
            "env" => std::env::var(name)
                .map_err(|_| format!("config {path}: ${{env:{name}}} is not set"))?,
            "file" => std::fs::read_to_string(name)
                .map_err(|e| format!("config {path}: ${{file:{name}}}: {e}"))?
                .trim_end_matches(['\r', '\n'])
                .to_string(),
            other => {
                return Err(format!(
                    "config {path}: unsupported reference kind '{other}' (env, file)"
                ))
            }
        };
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[derive(Clone, Default)]
pub struct Keyring {
    /// `(key, kid)`; the first entry seals.
    keys: Vec<([u8; 32], String)>,
}

fn parse_key(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    let bytes = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect::<Vec<_>>()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| "master key must be 64 hex chars or base64 of 32 bytes".to_string())?
    };
    <[u8; 32]>::try_from(bytes).map_err(|_| "master key must be exactly 32 bytes".to_string())
}

pub fn kid_of(key: &[u8; 32]) -> String {
    let d = Sha256::digest(key);
    d[..4].iter().map(|b| format!("{b:02x}")).collect()
}

impl Keyring {
    pub fn parse(master_keys: &[String]) -> Result<Self, String> {
        let mut keys = Vec::new();
        for k in master_keys {
            let key = parse_key(k)?;
            let kid = kid_of(&key);
            if !keys.iter().any(|(_, id)| *id == kid) {
                keys.push((key, kid));
            }
        }
        Ok(Keyring { keys })
    }

    pub fn kids(&self) -> Vec<String> {
        self.keys.iter().map(|(_, k)| k.clone()).collect()
    }

    pub fn current_kid(&self) -> Option<String> {
        self.keys.first().map(|(_, k)| k.clone())
    }

    pub fn seal(&self, plain: &str) -> String {
        let Some((key, kid)) = self.keys.first() else {
            return plain.to_string();
        };
        if plain.is_empty() || plain.starts_with(PREFIX) {
            return plain.to_string();
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
            .expect("chacha20poly1305 encrypt");
        let mut buf = nonce.to_vec();
        buf.extend_from_slice(&ct);
        format!(
            "{PREFIX}{kid}:{}",
            base64::engine::general_purpose::STANDARD.encode(buf)
        )
    }

    pub fn open(&self, s: &str) -> Result<String, String> {
        let Some(rest) = s.strip_prefix(PREFIX) else {
            return Ok(s.to_string());
        };
        let (kid, b64) = rest
            .split_once(':')
            .ok_or_else(|| "malformed sealed value".to_string())?;
        let (key, _) = self
            .keys
            .iter()
            .find(|(_, k)| k == kid)
            .ok_or_else(|| format!("sealed with unknown master key {kid}"))?;
        let buf = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| "malformed sealed value".to_string())?;
        if buf.len() < 12 + 16 {
            return Err("malformed sealed value".into());
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        let plain = cipher
            .decrypt(Nonce::from_slice(&buf[..12]), &buf[12..])
            .map_err(|_| format!("decryption with master key {kid} failed"))?;
        String::from_utf8(plain).map_err(|_| "sealed value is not UTF-8".to_string())
    }
}

fn ring() -> &'static RwLock<Keyring> {
    static R: OnceLock<RwLock<Keyring>> = OnceLock::new();
    R.get_or_init(|| RwLock::new(Keyring::default()))
}

/// Install the process key ring from `[security] master_keys`. Keys already installed stay
/// available for opening (they move behind the new ones); an empty list changes nothing.
pub fn install(master_keys: &[String]) -> Result<(), String> {
    let new = Keyring::parse(master_keys)?;
    if new.keys.is_empty() {
        return Ok(());
    }
    let mut r = ring().write().unwrap();
    let mut keys = new.keys;
    for (k, kid) in r.keys.drain(..) {
        if !keys.iter().any(|(_, id)| *id == kid) {
            keys.push((k, kid));
        }
    }
    r.keys = keys;
    Ok(())
}

pub fn configured() -> bool {
    !ring().read().unwrap().keys.is_empty()
}
pub fn kids() -> Vec<String> {
    ring().read().unwrap().kids()
}
pub fn current_kid() -> Option<String> {
    ring().read().unwrap().current_kid()
}
/// Seal with the current key (plaintext passthrough when no key is configured).
pub fn seal(plain: &str) -> String {
    ring().read().unwrap().seal(plain)
}
/// Open a stored value (plaintext passes through).
pub fn open(s: &str) -> Result<String, String> {
    ring().read().unwrap().open(s)
}
pub fn is_sealed(s: &str) -> bool {
    s.starts_with(PREFIX)
}
/// Key id a stored value was sealed with, if any.
pub fn sealed_kid(s: &str) -> Option<&str> {
    s.strip_prefix(PREFIX)?.split(':').next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_expand_env_and_file() {
        std::env::set_var("MUSUBI_TEST_REF", "s3cret");
        let dir = std::env::temp_dir().join(format!("musubi-ref-{}", std::process::id()));
        std::fs::write(&dir, "from-file\n").unwrap();
        let mut v = serde_json::json!({
            "database": { "url": "postgres://u:${env:MUSUBI_TEST_REF}@db/x" },
            "list": [ { "secret": format!("${{file:{}}}", dir.display()) } ],
            "plain": "no refs", "n": 3
        });
        let mut out = Vec::new();
        resolve_refs(&mut v, "", &mut out).unwrap();
        assert_eq!(v["database"]["url"], "postgres://u:s3cret@db/x");
        assert_eq!(v["list"][0]["secret"], "from-file");
        assert_eq!(out, vec!["database.url", "list[0].secret"]);
        let mut bad = serde_json::json!({ "a": { "b": "${env:MUSUBI_SURELY_UNSET_VAR}" } });
        let err = resolve_refs(&mut bad, "", &mut Vec::new()).unwrap_err();
        assert!(
            err.contains("a.b") && err.contains("MUSUBI_SURELY_UNSET_VAR"),
            "{err}"
        );
        assert!(expand("${vault:x}", "p").is_err());
        assert!(expand("${env:X", "p").is_err());
        std::fs::remove_file(dir).ok();
    }

    #[test]
    fn seal_open_rotate() {
        let k1 = "0".repeat(63) + "1";
        let k2 = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let r1 = Keyring::parse(std::slice::from_ref(&k1)).unwrap();
        assert!(Keyring::parse(&["short".into()]).is_err());
        let sealed = r1.seal("hunter2");
        assert!(sealed.starts_with(PREFIX) && sealed != "hunter2");
        assert_ne!(r1.seal("hunter2"), sealed, "fresh nonce every time");
        assert_eq!(r1.open(&sealed).unwrap(), "hunter2");
        assert_eq!(r1.open("plain").unwrap(), "plain");
        assert_eq!(r1.seal(""), "");
        assert_eq!(r1.seal(&sealed), sealed, "never double-seal");
        let r2 = Keyring::parse(std::slice::from_ref(&k2)).unwrap();
        assert!(r2.open(&sealed).unwrap_err().contains("unknown master key"));
        let both = Keyring::parse(&[k2, k1]).unwrap();
        assert_eq!(both.open(&sealed).unwrap(), "hunter2");
        assert_eq!(
            sealed_kid(&both.seal("x")).unwrap(),
            both.current_kid().unwrap()
        );
        assert_eq!(both.kids().len(), 2);
        let empty = Keyring::default();
        assert_eq!(empty.seal("x"), "x");
    }
}
