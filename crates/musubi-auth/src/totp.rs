//! RFC 6238 TOTP (SHA-1, 30 s, 6 digits) and RFC 4648 base32 for authenticator apps.

use ring::hmac;
use subtle::ConstantTimeEq;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
pub const STEP_SECS: i64 = 30;
pub const DIGITS: u32 = 6;

pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        if c == b'=' || c == b' ' || c == b'-' {
            continue;
        }
        let v = ALPHABET.iter().position(|&a| a == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// 20 random bytes, as the authenticator wants them.
pub fn new_secret() -> String {
    use rand::RngCore;
    let mut b = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut b);
    base32_encode(&b)
}

pub fn hotp(secret: &[u8], counter: u64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &counter.to_be_bytes());
    let h = tag.as_ref();
    let off = (h[19] & 0x0f) as usize;
    let bin = ((h[off] as u32 & 0x7f) << 24)
        | ((h[off + 1] as u32) << 16)
        | ((h[off + 2] as u32) << 8)
        | h[off + 3] as u32;
    bin % 10u32.pow(DIGITS)
}

/// The code for `secret` (base32) at unix time `now`.
pub fn code_at(secret_b32: &str, now: i64) -> Option<String> {
    let secret = base32_decode(secret_b32)?;
    Some(format!(
        "{:0width$}",
        hotp(&secret, (now / STEP_SECS) as u64),
        width = DIGITS as usize
    ))
}

/// Accepts the current step and one step either side (clock skew).
pub fn verify(secret_b32: &str, code: &str, now: i64) -> bool {
    verify_step(secret_b32, code, now).is_some()
}

/// Like [`verify`], but returns the matching time step (`now / STEP_SECS` ± 1) so the caller
/// can refuse a code whose step was already accepted (RFC 6238 §5.2 replay guard). The
/// comparison is constant-time.
pub fn verify_step(secret_b32: &str, code: &str, now: i64) -> Option<i64> {
    let code = code.trim();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secret = base32_decode(secret_b32)?;
    [-1i64, 0, 1]
        .iter()
        .map(|d| now / STEP_SECS + d)
        .find(|step| {
            let want = format!(
                "{:0width$}",
                hotp(&secret, *step as u64),
                width = DIGITS as usize
            );
            bool::from(want.as_bytes().ct_eq(code.as_bytes()))
        })
}

/// `otpauth://totp/<issuer>:<label>?secret=…&issuer=…` for QR codes.
pub fn otpauth_uri(issuer: &str, label: &str, secret_b32: &str) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={}&period={}",
        enc(issuer),
        enc(label),
        secret_b32,
        enc(issuer),
        DIGITS,
        STEP_SECS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_step_reports_the_matching_step_for_replay_guards() {
        let secret = new_secret();
        let now = 1_700_000_000i64;
        let code = code_at(&secret, now).unwrap();
        assert_eq!(verify_step(&secret, &code, now), Some(now / STEP_SECS));
        // skew: the same code is the previous step a window later and the next step a window earlier
        assert_eq!(
            verify_step(&secret, &code, now + STEP_SECS),
            Some(now / STEP_SECS)
        );
        assert_eq!(
            verify_step(&secret, &code, now - STEP_SECS),
            Some(now / STEP_SECS)
        );
        assert_eq!(verify_step(&secret, &code, now + 2 * STEP_SECS), None);
        assert_eq!(verify_step(&secret, "12345", now), None, "wrong length");
        assert_eq!(verify_step(&secret, "12345a", now), None, "non-digit");
        assert_eq!(verify_step("!!not-base32!!", &code, now), None);
        assert!(verify(&secret, &format!(" {code} "), now), "trimmed");
    }

    #[test]
    fn rfc6238_sha1_vectors() {
        // RFC 6238 appendix B, secret "12345678901234567890"
        let secret = base32_encode(b"12345678901234567890");
        assert_eq!(secret, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        for (t, want) in [
            (59, "287082"),
            (1111111109, "081804"),
            (1234567890, "005924"),
        ] {
            // RFC vectors are 8 digits; ours are the last 6
            assert_eq!(code_at(&secret, t).unwrap(), want[want.len() - 6..]);
        }
        assert!(verify(&secret, "287082", 59));
        assert!(verify(&secret, "287082", 59 + 30));
        assert!(!verify(&secret, "287082", 59 + 61));
        assert_eq!(base32_decode(&secret).unwrap(), b"12345678901234567890");
    }
}
