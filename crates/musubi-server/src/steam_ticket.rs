//! Steam encrypted application tickets (`ISteamUser::RequestEncryptedAppTicket`), verified with
//! the title's SDK Auth key — what Valve's `sdkencryptedappticket` library does, in Rust, so a
//! backend without Steam egress (or without a Web API key) can still prove who a client is.
//!
//! Wire format: a protobuf envelope around `AES-256` ciphertext (the first 16 bytes are the IV
//! encrypted with ECB, the rest CBC/PKCS7). The plaintext is `user_data || ownership ticket ||
//! [v2 extras] || salt(8) || sha1(prefix || salt)`, checked by CRC32 of the whole plaintext.

use musubi_core::{Error, Result};
use serde::Serialize;

#[derive(Clone, PartialEq, prost::Message)]
struct Envelope {
    #[prost(uint32, optional, tag = "1")]
    ticket_version_no: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    crc_encryptedticket: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    cb_encrypteduserdata: Option<u32>,
    #[prost(uint32, optional, tag = "4")]
    cb_encrypted_appownershipticket: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "5")]
    encrypted_ticket: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Dlc {
    pub app_id: u32,
    pub licenses: Vec<u32>,
}

/// A decrypted, integrity-checked app ticket.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppTicket {
    pub version: u32,
    /// SteamID64 as a string.
    pub steam_id: String,
    pub app_id: u32,
    pub external_ip: String,
    pub internal_ip: String,
    pub ownership_flags: u32,
    /// Unix seconds the ticket was issued (`SteamEncryptedAppTicket_GetTicketIssueTime`).
    pub issued_at: i64,
    /// Package ids licensed to the user for this app.
    pub licenses: Vec<u32>,
    pub dlc: Vec<Dlc>,
    /// `ISteamUser::RequestEncryptedAppTicket(data)` payload, base64.
    pub user_data: String,
    #[serde(skip)]
    pub user_data_raw: Vec<u8>,
}

fn bad(what: &str) -> Error {
    Error::Unauthenticated(format!("steam app ticket: {what}"))
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| bad("truncated"))?;
        let s = self.b.get(self.at..end).ok_or_else(|| bad("truncated"))?;
        self.at = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

fn ipv4(n: u32) -> String {
    std::net::Ipv4Addr::from(n).to_string()
}

/// Steam's symmetric scheme: `ECB(key, iv) || CBC(key, iv, PKCS7(plaintext))`.
fn symmetric_decrypt(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit};
    if data.len() < 32 || (data.len() - 16) % 16 != 0 {
        return Err(bad("ciphertext length"));
    }
    let ecb = aes::Aes256::new(key.into());
    let mut iv = aes::cipher::generic_array::GenericArray::clone_from_slice(&data[..16]);
    ecb.decrypt_block(&mut iv);
    cbc::Decryptor::<aes::Aes256>::new(key.into(), &iv)
        .decrypt_padded_vec_mut::<Pkcs7>(&data[16..])
        .map_err(|_| bad("padding"))
}

#[doc(hidden)]
pub fn symmetric_encrypt(plain: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Vec<u8> {
    use aes::cipher::{block_padding::Pkcs7, BlockEncrypt, BlockEncryptMut, KeyInit, KeyIvInit};
    let ecb = aes::Aes256::new(key.into());
    let mut head = aes::cipher::generic_array::GenericArray::clone_from_slice(iv);
    ecb.encrypt_block(&mut head);
    let mut out = head.to_vec();
    out.extend(
        cbc::Encryptor::<aes::Aes256>::new(key.into(), iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plain),
    );
    out
}

fn parse_ownership(t: &[u8]) -> Result<AppTicket> {
    let mut c = Cursor { b: t, at: 0 };
    let len = c.u32()? as usize;
    if len != t.len() && len + 128 != t.len() {
        return Err(bad("ownership ticket length"));
    }
    let version = c.u32()?;
    let steam_id = c.u64()?;
    let app_id = c.u32()?;
    let external_ip = ipv4(c.u32()?);
    let internal_ip = ipv4(c.u32()?);
    let ownership_flags = c.u32()?;
    let issued_at = c.u32()? as i64;
    let _expires = c.u32()?;
    let n = c.u16()? as usize;
    let mut licenses = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        licenses.push(c.u32()?);
    }
    let n = c.u16()? as usize;
    let mut dlc = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        let dlc_app = c.u32()?;
        let m = c.u16()? as usize;
        let mut l = Vec::with_capacity(m.min(256));
        for _ in 0..m {
            l.push(c.u32()?);
        }
        dlc.push(Dlc {
            app_id: dlc_app,
            licenses: l,
        });
    }
    let _reserved = c.u16()?;
    if c.at != len {
        return Err(bad("ownership ticket layout"));
    }
    Ok(AppTicket {
        version,
        steam_id: steam_id.to_string(),
        app_id,
        external_ip,
        internal_ip,
        ownership_flags,
        issued_at,
        licenses,
        dlc,
        user_data: String::new(),
        user_data_raw: Vec::new(),
    })
}

/// Decrypts and verifies a raw (binary) encrypted app ticket with the 32-byte SDK Auth key.
pub fn decrypt(ticket: &[u8], key: &[u8; 32]) -> Result<AppTicket> {
    use base64::Engine;
    use prost::Message;
    let env = Envelope::decode(ticket).map_err(|_| bad("not an encrypted app ticket"))?;
    let enc = env.encrypted_ticket.as_deref().unwrap_or_default();
    let plain = symmetric_decrypt(enc, key)?;
    if crc32fast::hash(&plain) != env.crc_encryptedticket.unwrap_or_default() {
        return Err(bad("wrong key or corrupt ticket"));
    }
    let cb_user = env.cb_encrypteduserdata.unwrap_or_default() as usize;
    let user = plain
        .get(..cb_user)
        .ok_or_else(|| bad("user data length"))?;
    let own_len = plain
        .get(cb_user..cb_user + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or_else(|| bad("truncated"))?;
    if let Some(cb) = env.cb_encrypted_appownershipticket {
        if cb as usize != own_len {
            return Err(bad("ownership ticket length"));
        }
    }
    let own = plain
        .get(cb_user..cb_user + own_len)
        .ok_or_else(|| bad("truncated"))?;
    let mut t = parse_ownership(own)?;
    let mut prefix = cb_user + own_len;
    if env.ticket_version_no == Some(2) {
        prefix += 8 + 8 + 4;
    }
    if plain.len() < prefix {
        return Err(bad("truncated"));
    }
    let rest = &plain[prefix..];
    if rest.len() >= 28 {
        use sha1::Digest;
        let mut h = sha1::Sha1::new();
        h.update(&plain[..prefix]);
        h.update(&rest[..8]);
        if h.finalize().as_slice() != &rest[8..28] {
            return Err(bad("signature"));
        }
    }
    t.user_data = base64::engine::general_purpose::STANDARD.encode(user);
    t.user_data_raw = user.to_vec();
    Ok(t)
}

/// Parses the client-supplied text form: hex (what `BinToHex` on the SDK buffer gives) or base64.
pub fn decode_text(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    let s = s.trim();
    if s.is_empty() || s.len() > 16 * 1024 {
        return Err(bad("empty or oversized"));
    }
    if s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return hex::decode(s).map_err(|_| bad("not hex/base64"));
    }
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s))
        .map_err(|_| bad("not hex/base64"))
}

/// Test fixture: what the Steam client would put into a ticket.
#[doc(hidden)]
pub struct Fixture {
    pub steam_id: u64,
    pub app_id: u32,
    pub issued_at: u32,
    pub licenses: Vec<u32>,
    pub dlc: Vec<Dlc>,
    pub user_data: Vec<u8>,
    pub signed: bool,
    pub version: u32,
}

/// Builds a ticket the way the Steam client does (test fixture).
#[doc(hidden)]
pub fn encrypt(f: &Fixture, key: &[u8; 32]) -> Vec<u8> {
    use prost::Message;
    let mut own = Vec::new();
    own.extend(0u32.to_le_bytes());
    own.extend(4u32.to_le_bytes());
    own.extend(f.steam_id.to_le_bytes());
    own.extend(f.app_id.to_le_bytes());
    own.extend(u32::from(std::net::Ipv4Addr::new(203, 0, 113, 7)).to_le_bytes());
    own.extend(u32::from(std::net::Ipv4Addr::new(10, 0, 0, 2)).to_le_bytes());
    own.extend(1u32.to_le_bytes());
    own.extend(f.issued_at.to_le_bytes());
    own.extend((f.issued_at + 3600 * 24 * 21).to_le_bytes());
    own.extend((f.licenses.len() as u16).to_le_bytes());
    for l in &f.licenses {
        own.extend(l.to_le_bytes());
    }
    own.extend((f.dlc.len() as u16).to_le_bytes());
    for d in &f.dlc {
        own.extend(d.app_id.to_le_bytes());
        own.extend((d.licenses.len() as u16).to_le_bytes());
        for l in &d.licenses {
            own.extend(l.to_le_bytes());
        }
    }
    own.extend(0u16.to_le_bytes());
    let len = own.len() as u32;
    own[..4].copy_from_slice(&len.to_le_bytes());
    let mut plain = f.user_data.clone();
    plain.extend(&own);
    if f.version == 2 {
        plain.extend([0u8; 20]);
    }
    if f.signed {
        use sha1::Digest;
        let salt = [7u8; 8];
        let mut h = sha1::Sha1::new();
        h.update(&plain);
        h.update(salt);
        let hash = h.finalize();
        plain.extend(salt);
        plain.extend(hash);
        plain.extend(42u32.to_le_bytes());
    }
    let crc = crc32fast::hash(&plain);
    let enc = symmetric_encrypt(&plain, key, &[0x11; 16]);
    Envelope {
        ticket_version_no: Some(f.version),
        crc_encryptedticket: Some(crc),
        cb_encrypteduserdata: Some(f.user_data.len() as u32),
        cb_encrypted_appownershipticket: Some(len),
        encrypted_ticket: Some(enc),
    }
    .encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x5a; 32];

    fn fixture() -> Fixture {
        Fixture {
            steam_id: 76561198012345678,
            app_id: 480,
            issued_at: 1_700_000_000,
            licenses: vec![1234, 5678],
            dlc: vec![Dlc {
                app_id: 481,
                licenses: vec![9],
            }],
            user_data: b"hello".to_vec(),
            signed: true,
            version: 1,
        }
    }

    #[test]
    fn round_trip_and_tamper_detection() {
        let f = fixture();
        let raw = encrypt(&f, &KEY);
        let t = decrypt(&raw, &KEY).unwrap();
        assert_eq!(t.steam_id, "76561198012345678");
        assert_eq!(t.app_id, 480);
        assert_eq!(t.issued_at, 1_700_000_000);
        assert_eq!(t.licenses, vec![1234, 5678]);
        assert_eq!(t.dlc[0].app_id, 481);
        assert_eq!(t.user_data_raw, b"hello");
        assert_eq!(t.external_ip, "203.0.113.7");
        assert_eq!(t.user_data, "aGVsbG8=");
        // text forms
        let hex_s = hex::encode(&raw);
        assert_eq!(decode_text(&hex_s).unwrap(), raw);
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert_eq!(decode_text(&b64).unwrap(), raw);
        assert!(decode_text("").is_err());
        assert!(decode_text("!!!").is_err());
        // wrong key → crc mismatch (or padding)
        assert!(decrypt(&raw, &[1u8; 32]).is_err());
        // flipped ciphertext byte
        let mut bad_raw = raw.clone();
        let n = bad_raw.len();
        bad_raw[n - 3] ^= 0x80;
        assert!(decrypt(&bad_raw, &KEY).is_err());
        // garbage
        assert!(decrypt(b"nope", &KEY).is_err());
        assert!(decrypt(&[], &KEY).is_err());
        // unsigned (no sha1 trailer) and v2 extras, empty user data
        let mut g = fixture();
        g.signed = false;
        g.user_data.clear();
        g.version = 2;
        let t2 = decrypt(&encrypt(&g, &KEY), &KEY).unwrap();
        assert_eq!(t2.steam_id, t.steam_id);
        assert!(t2.user_data_raw.is_empty());
    }

    #[test]
    fn forged_signature_is_rejected() {
        // re-encrypt a plaintext whose sha1 trailer does not match (crc recomputed, so only the
        // salted hash catches it)
        let f = fixture();
        let raw = encrypt(&f, &KEY);
        let mut plain = {
            use prost::Message;
            let env = Envelope::decode(raw.as_slice()).unwrap();
            symmetric_decrypt(env.encrypted_ticket.as_deref().unwrap(), &KEY).unwrap()
        };
        let n = plain.len();
        plain[n - 10] ^= 1; // inside the sha1
        use prost::Message;
        let env = Envelope {
            ticket_version_no: Some(1),
            crc_encryptedticket: Some(crc32fast::hash(&plain)),
            cb_encrypteduserdata: Some(5),
            cb_encrypted_appownershipticket: None,
            encrypted_ticket: Some(symmetric_encrypt(&plain, &KEY, &[3; 16])),
        }
        .encode_to_vec();
        let e = decrypt(&env, &KEY).unwrap_err().to_string();
        assert!(e.contains("signature"), "{e}");
    }
}
