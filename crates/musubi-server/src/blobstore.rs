//! S3-compatible object storage for large blobs (UGC): SigV4-signed PUT/GET/DELETE over plain
//! HTTP, path-style (`endpoint/bucket/key`) or virtual-host-style URLs. Works with AWS S3, MinIO,
//! Cloudflare R2, Backblaze B2, GCS in interoperability mode.

use hmac::{Hmac, Mac};
use musubi_core::config::S3Config;
use musubi_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub struct S3Store {
    cfg: S3Config,
    client: reqwest::Client,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

fn amz_date(t: time::OffsetDateTime) -> (String, String) {
    let date = format!("{:04}{:02}{:02}", t.year(), t.month() as u8, t.day());
    let full = format!("{date}T{:02}{:02}{:02}Z", t.hour(), t.minute(), t.second());
    (date, full)
}

fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl S3Store {
    /// `None` when no bucket is configured.
    pub fn new(cfg: &S3Config) -> Option<Self> {
        if cfg.bucket.is_empty() || cfg.endpoint.is_empty() {
            return None;
        }
        Some(Self {
            cfg: cfg.clone(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client"),
        })
    }

    pub fn bucket(&self) -> &str {
        &self.cfg.bucket
    }

    fn url_and_path(&self, key: &str) -> (String, String, String) {
        let endpoint = self.cfg.endpoint.trim_end_matches('/');
        let key = uri_encode(key.trim_start_matches('/'));
        if self.cfg.path_style {
            let path = format!("/{}/{key}", self.cfg.bucket);
            let host = endpoint.split("://").nth(1).unwrap_or(endpoint).to_string();
            (format!("{endpoint}{path}"), host, path)
        } else {
            let (scheme, host) = endpoint.split_once("://").unwrap_or(("https", endpoint));
            let host = format!("{}.{host}", self.cfg.bucket);
            let path = format!("/{key}");
            (format!("{scheme}://{host}{path}"), host, path)
        }
    }

    /// SigV4 (`s3`, header signing, signed payload) for one request.
    pub fn sign(
        &self,
        method: &str,
        host: &str,
        path: &str,
        query: &str,
        payload_sha: &str,
        now: time::OffsetDateTime,
    ) -> (String, String) {
        let (date, amz) = amz_date(now);
        let mut headers = vec![
            ("host".to_string(), host.to_string()),
            ("x-amz-content-sha256".to_string(), payload_sha.to_string()),
            ("x-amz-date".to_string(), amz.clone()),
        ];
        if !self.cfg.session_token.is_empty() {
            headers.push((
                "x-amz-security-token".to_string(),
                self.cfg.session_token.clone(),
            ));
        }
        headers.sort();
        let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed_headers = headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical = format!(
            "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_sha}"
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.cfg.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}",
            sha256_hex(canonical.as_bytes())
        );
        let k_date = hmac(
            format!("AWS4{}", self.cfg.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = hmac(&k_date, self.cfg.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
        (
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                self.cfg.access_key
            ),
            amz,
        )
    }

    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        query: &str,
        body: Option<(Vec<u8>, &str)>,
    ) -> Result<reqwest::Response> {
        let (mut url, host, path) = self.url_and_path(key);
        if !query.is_empty() {
            url = format!("{url}?{query}");
        }
        let payload_sha = sha256_hex(body.as_ref().map(|(b, _)| b.as_slice()).unwrap_or(&[]));
        let (auth, amz) = self.sign(
            method.as_str(),
            &host,
            &path,
            query,
            &payload_sha,
            time::OffsetDateTime::now_utc(),
        );
        let mut req = self
            .client
            .request(method, &url)
            .header("authorization", auth)
            .header("x-amz-date", amz)
            .header("x-amz-content-sha256", payload_sha);
        if !self.cfg.session_token.is_empty() {
            req = req.header("x-amz-security-token", &self.cfg.session_token);
        }
        if let Some((bytes, mime)) = body {
            req = req.header("content-type", mime).body(bytes);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("object storage: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::NotFound(format!("object {key}")));
        }
        if !resp.status().is_success() {
            let st = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Unavailable(format!(
                "object storage {st}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        Ok(resp)
    }

    pub async fn put(&self, key: &str, bytes: Vec<u8>, mime: &str) -> Result<()> {
        self.send(reqwest::Method::PUT, key, "", Some((bytes, mime)))
            .await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let resp = self.send(reqwest::Method::GET, key, "", None).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Unavailable(format!("object storage: {e}")))
    }

    /// The object as a byte stream (plus its length when the store reports one) — downloads are
    /// proxied without buffering.
    pub async fn get_stream(
        &self,
        key: &str,
    ) -> Result<(
        Option<u64>,
        impl futures::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>>,
    )> {
        use futures::TryStreamExt;
        let resp = self.send(reqwest::Method::GET, key, "", None).await?;
        let len = resp.content_length();
        Ok((len, resp.bytes_stream().map_err(std::io::Error::other)))
    }

    /// Multipart upload: begin → `upload_part` (≥ 5 MiB each on AWS, any size on the last) →
    /// `complete`; `abort` drops the parts of an upload that will not be completed.
    pub async fn begin_multipart(&self, key: &str, mime: &str) -> Result<String> {
        let resp = self
            .send(
                reqwest::Method::POST,
                key,
                "uploads=",
                Some((Vec::new(), mime)),
            )
            .await?;
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Unavailable(format!("object storage: {e}")))?;
        let start = text.find("<UploadId>").map(|i| i + "<UploadId>".len());
        let end = text.find("</UploadId>");
        match (start, end) {
            (Some(a), Some(b)) if a <= b => Ok(text[a..b].trim().to_string()),
            _ => Err(Error::Unavailable(
                "object storage: multipart begin without UploadId".into(),
            )),
        }
    }

    pub async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        number: u32,
        bytes: Vec<u8>,
    ) -> Result<String> {
        let query = format!("partNumber={number}&uploadId={}", uri_encode(upload_id));
        let resp = self
            .send(
                reqwest::Method::PUT,
                key,
                &query,
                Some((bytes, "application/octet-stream")),
            )
            .await?;
        Ok(resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string())
    }

    pub async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[(u32, String)],
    ) -> Result<()> {
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (n, etag) in parts {
            let etag = etag.replace('&', "&amp;").replace('<', "&lt;");
            xml.push_str(&format!(
                "<Part><PartNumber>{n}</PartNumber><ETag>{etag}</ETag></Part>"
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let query = format!("uploadId={}", uri_encode(upload_id));
        self.send(
            reqwest::Method::POST,
            key,
            &query,
            Some((xml.into_bytes(), "application/xml")),
        )
        .await?;
        Ok(())
    }

    pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<()> {
        let query = format!("uploadId={}", uri_encode(upload_id));
        match self.send(reqwest::Method::DELETE, key, &query, None).await {
            Ok(_) | Err(Error::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        match self.send(reqwest::Method::DELETE, key, "", None).await {
            Ok(_) | Err(Error::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_deterministic_and_well_formed() {
        let s = S3Store::new(&S3Config {
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "ugc".into(),
            region: "us-east-1".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            ..Default::default()
        })
        .unwrap();
        let now = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let (url, host, path) = s.url_and_path("ugc/a b/x");
        assert_eq!(url, "http://127.0.0.1:9000/ugc/ugc/a%20b/x");
        assert_eq!(host, "127.0.0.1:9000");
        assert_eq!(path, "/ugc/ugc/a%20b/x");
        let (a1, amz) = s.sign("PUT", &host, &path, "", &sha256_hex(b"hi"), now);
        let (a2, _) = s.sign("PUT", &host, &path, "", &sha256_hex(b"hi"), now);
        assert_eq!(a1, a2);
        assert_eq!(amz, "20231114T221320Z");
        assert!(a1.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20231114/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="));
        assert!(S3Store::new(&S3Config::default()).is_none());
    }
}
