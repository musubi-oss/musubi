//! Outgoing e-mail over SMTP (RFC 5321) with STARTTLS / implicit TLS and AUTH PLAIN. Used for
//! `account.email_verification` and `account.password_reset`; the events still reach webhooks and
//! modules, so a game may bring its own mailer instead.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use musubi_core::config::EmailConfig;
use musubi_core::{Error, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub struct Mailer {
    cfg: EmailConfig,
    tls: Arc<rustls::ClientConfig>,
}

/// One message; `text` is the plain-text body.
pub struct Mail<'a> {
    pub to: &'a str,
    pub subject: &'a str,
    pub text: &'a str,
}

impl Mailer {
    pub fn new(cfg: EmailConfig) -> Self {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self {
            cfg,
            tls: Arc::new(tls),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.cfg.host.is_empty()
    }

    pub fn config(&self) -> &EmailConfig {
        &self.cfg
    }

    /// Address of the verification / reset page with `{token}` substituted; empty when the game
    /// has not configured a page (the mail then carries the bare token).
    pub fn link(template: &str, token: &str) -> Option<String> {
        if template.is_empty() {
            None
        } else {
            Some(template.replace("{token}", token))
        }
    }

    pub async fn send(&self, mail: Mail<'_>) -> Result<()> {
        if !self.enabled() {
            return Err(Error::Unavailable("smtp is not configured".into()));
        }
        check_address(mail.to)?;
        check_address(&self.cfg.from)?;
        let timeout = Duration::from_secs(self.cfg.timeout_secs.max(1));
        tokio::time::timeout(timeout, self.send_inner(mail))
            .await
            .map_err(|_| {
                tracing::warn!(host = %self.cfg.host, port = self.cfg.port, "smtp: timed out");
                Error::Unavailable("smtp: timed out".into())
            })?
    }

    async fn send_inner(&self, mail: Mail<'_>) -> Result<()> {
        let addr = format!("{}:{}", self.cfg.host, self.cfg.port);
        let tcp = TcpStream::connect(&addr).await.map_err(|e| {
            tracing::warn!(%addr, error = %e, "smtp: connect failed");
            Error::Unavailable("smtp: connect failed".into())
        })?;
        let message = self.message(&mail);
        match self.cfg.tls.as_str() {
            "none" => {
                let mut s = BufReader::new(tcp);
                expect(&mut s, 220).await?;
                self.session(&mut s, mail.to, &message).await
            }
            "starttls" => {
                let mut s = BufReader::new(tcp);
                expect(&mut s, 220).await?;
                command(&mut s, &format!("EHLO {}", self.cfg.helo), 250).await?;
                command(&mut s, "STARTTLS", 220).await?;
                let tcp = s.into_inner();
                let mut s = BufReader::new(self.upgrade(tcp).await?);
                self.session(&mut s, mail.to, &message).await
            }
            "implicit" => {
                let mut s = BufReader::new(self.upgrade(tcp).await?);
                expect(&mut s, 220).await?;
                self.session(&mut s, mail.to, &message).await
            }
            other => Err(Error::InvalidArgument(format!(
                "email.tls must be starttls, implicit or none (got {other:?})"
            ))),
        }
    }

    async fn upgrade(&self, tcp: TcpStream) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let name = rustls_pki_types::ServerName::try_from(self.cfg.host.clone())
            .map_err(|e| Error::InvalidArgument(format!("email.host: {e}")))?;
        tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .map_err(|e| {
                tracing::warn!(host = %self.cfg.host, error = %e, "smtp: tls handshake failed");
                Error::Unavailable("smtp: tls handshake failed".into())
            })
    }

    /// EHLO → AUTH → MAIL/RCPT/DATA → QUIT on an established (plain or TLS) stream.
    async fn session<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        s: &mut BufReader<S>,
        to: &str,
        message: &str,
    ) -> Result<()> {
        command(s, &format!("EHLO {}", self.cfg.helo), 250).await?;
        if !self.cfg.username.is_empty() {
            let plain = format!("\0{}\0{}", self.cfg.username, self.cfg.password);
            let b64 = base64::engine::general_purpose::STANDARD.encode(plain.as_bytes());
            command(s, &format!("AUTH PLAIN {b64}"), 235)
                .await
                .map_err(|e| Error::Unauthenticated(format!("smtp auth: {e}")))?;
        }
        command(s, &format!("MAIL FROM:<{}>", self.cfg.from), 250).await?;
        command(s, &format!("RCPT TO:<{to}>"), 250).await?;
        command(s, "DATA", 354).await?;
        s.write_all(message.as_bytes())
            .await
            .map_err(|e| Error::Unavailable(format!("smtp: {e}")))?;
        s.write_all(b"\r\n.\r\n")
            .await
            .map_err(|e| Error::Unavailable(format!("smtp: {e}")))?;
        expect(s, 250).await?;
        let _ = command(s, "QUIT", 221).await;
        Ok(())
    }

    fn message(&self, mail: &Mail<'_>) -> String {
        let from = if self.cfg.from_name.is_empty() {
            format!("<{}>", self.cfg.from)
        } else {
            format!("{} <{}>", encode_word(&self.cfg.from_name), self.cfg.from)
        };
        let date = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc2822)
            .unwrap_or_default();
        let domain = self.cfg.from.rsplit('@').next().unwrap_or("localhost");
        let id = format!("<{}@{domain}>", uuid_like());
        let mut body = String::new();
        for line in mail.text.replace("\r\n", "\n").split('\n') {
            if line.starts_with('.') {
                body.push('.');
            }
            body.push_str(line);
            body.push_str("\r\n");
        }
        format!(
            "From: {from}\r\nTo: <{}>\r\nSubject: {}\r\nDate: {date}\r\nMessage-ID: {id}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}",
            mail.to,
            encode_word(mail.subject)
        )
    }
}

fn uuid_like() -> String {
    let mut b = [0u8; 16];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// RFC 2047 encoded-word for non-ASCII header values; ASCII passes through.
fn encode_word(s: &str) -> String {
    if s.is_ascii() && !s.contains(['\r', '\n']) {
        s.to_string()
    } else {
        let clean = s.replace(['\r', '\n'], " ");
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(clean.as_bytes())
        )
    }
}

fn check_address(a: &str) -> Result<()> {
    let ok = !a.is_empty()
        && a.len() <= 254
        && a.contains('@')
        && !a.contains([' ', '<', '>', '\r', '\n', '\0']);
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!("bad e-mail address {a:?}")))
    }
}

/// RFC 5321 §4.5.3.1.5 allows 512 octets per reply line; be lenient but bounded so a hostile
/// relay cannot make us buffer an endless line or an endless multi-line reply.
const MAX_REPLY_LINE_BYTES: u64 = 2048;
const MAX_REPLY_LINES: usize = 64;

/// Read one (possibly multi-line) reply; Ok(text) when its code matches.
async fn expect<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut BufReader<S>,
    code: u16,
) -> Result<String> {
    let mut text = String::new();
    for _ in 0..MAX_REPLY_LINES {
        let mut line = String::new();
        let n = tokio::io::AsyncReadExt::take(&mut *s, MAX_REPLY_LINE_BYTES)
            .read_line(&mut line)
            .await
            .map_err(|e| Error::Unavailable(format!("smtp: read: {e}")))?;
        if n == 0 {
            return Err(Error::Unavailable("smtp: connection closed".into()));
        }
        if !line.ends_with('\n') {
            return Err(Error::Unavailable("smtp: reply line too long".into()));
        }
        text.push_str(line.trim_end());
        text.push('\n');
        let got = reply_code(&line)
            .ok_or_else(|| Error::Unavailable(format!("smtp: bad reply {line:?}")))?;
        if line.as_bytes()[3] == b'-' {
            continue;
        }
        if got != code {
            return Err(Error::Unavailable(format!(
                "smtp: expected {code}, got {}",
                text.trim_end()
            )));
        }
        return Ok(text);
    }
    Err(Error::Unavailable("smtp: reply has too many lines".into()))
}

/// The 3-digit reply code of a line that is at least `ddd<sep>` long; works on bytes so a reply
/// starting with multi-byte UTF-8 cannot panic on a char boundary.
fn reply_code(line: &str) -> Option<u16> {
    let b = line.as_bytes();
    if b.len() < 4 || !b[..3].iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(u16::from(b[0] - b'0') * 100 + u16::from(b[1] - b'0') * 10 + u16::from(b[2] - b'0'))
}

async fn command<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut BufReader<S>,
    line: &str,
    code: u16,
) -> Result<String> {
    s.write_all(format!("{line}\r\n").as_bytes())
        .await
        .map_err(|e| Error::Unavailable(format!("smtp: write: {e}")))?;
    expect(s, code).await
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn reply(bytes: &[u8], code: u16) -> Result<String> {
        let (client, mut server) = tokio::io::duplex(1 << 16);
        server.write_all(bytes).await.unwrap();
        drop(server);
        let mut s = BufReader::new(client);
        expect(&mut s, code).await
    }

    #[test]
    fn reply_code_is_byte_safe() {
        assert_eq!(reply_code("220 ok\r\n"), Some(220));
        assert_eq!(reply_code("250-x\r\n"), Some(250));
        assert_eq!(
            reply_code("Ω\r\n"),
            None,
            "multi-byte prefix must not panic"
        );
        assert_eq!(reply_code("ΩΩ ok\r\n"), None);
        assert_eq!(reply_code("22a ok\r\n"), None);
        assert_eq!(reply_code("22\n"), None);
    }

    #[tokio::test]
    async fn multiline_and_malformed_replies() {
        let ok = reply(b"250-first\r\n250-second\r\n250 done\r\n", 250)
            .await
            .unwrap();
        assert_eq!(ok, "250-first\n250-second\n250 done\n");
        assert!(reply(b"550 nope\r\n", 250).await.is_err());
        assert!(reply("ΩΩΩΩ\r\n".as_bytes(), 220).await.is_err());
        assert!(reply(b"", 220).await.is_err());
    }

    #[tokio::test]
    async fn hostile_replies_are_bounded() {
        let long = format!("220 {}\r\n", "x".repeat(MAX_REPLY_LINE_BYTES as usize));
        let e = reply(long.as_bytes(), 220).await.unwrap_err().to_string();
        assert!(e.contains("too long"), "{e}");
        let endless = "250-again\r\n".repeat(MAX_REPLY_LINES + 1);
        let e = reply(endless.as_bytes(), 250)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("too many lines"), "{e}");
    }
}
