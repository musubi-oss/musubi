//! Container-registry reachability check for fleet images (Edgegap `verify_image` parity):
//! resolve `registry/repo:tag|@digest`, ask the Docker Registry v2 / OCI distribution API for the
//! manifest (anonymous first, then the `WWW-Authenticate: Bearer realm=… service=… scope=…` token
//! dance, with basic credentials when given) and return the manifest digest.

use musubi_core::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// `https://registry-1.docker.io` (docker.io shorthand) or `http://127.0.0.1:5000` for
    /// loopback/plain-port dev registries.
    pub registry: String,
    pub repository: String,
    /// Tag or `sha256:…` digest.
    pub reference: String,
}

/// Registries reached over plaintext HTTP: local and private-network hosts only.
fn plain_http_host(bare: &str) -> bool {
    bare == "localhost"
        || bare == "127.0.0.1"
        || bare.starts_with("10.")
        || bare.starts_with("192.168.")
}

pub fn parse_image(image: &str) -> Result<ImageRef> {
    let image = image.trim();
    if image.is_empty() || image.len() > 512 || image.chars().any(char::is_whitespace) {
        return Err(Error::InvalidArgument(
            "spec.image: empty or malformed".into(),
        ));
    }
    let (name, reference) = match image.rsplit_once('@') {
        Some((n, d)) if d.starts_with("sha256:") => (n.to_string(), d.to_string()),
        _ => {
            // the last `:` after the last `/` is a tag; a `:` before the first `/` is a port
            let last_slash = image.rfind('/').map(|i| i + 1).unwrap_or(0);
            match image[last_slash..].rfind(':') {
                Some(i) => (
                    image[..last_slash + i].to_string(),
                    image[last_slash + i + 1..].to_string(),
                ),
                None => (image.to_string(), "latest".to_string()),
            }
        }
    };
    let (host, repo) = match name.split_once('/') {
        Some((h, r)) if h.contains('.') || h.contains(':') || h == "localhost" => {
            (h.to_string(), r.to_string())
        }
        _ => ("docker.io".to_string(), name.clone()),
    };
    let repository = if host == "docker.io" && !repo.contains('/') {
        format!("library/{repo}")
    } else {
        repo
    };
    if repository.is_empty() || reference.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "spec.image `{image}`: expected registry/repository:tag"
        )));
    }
    let bare = host.split(':').next().unwrap_or("");
    let plain = plain_http_host(bare);
    let registry = match host.as_str() {
        "docker.io" | "index.docker.io" => "https://registry-1.docker.io".to_string(),
        h if plain => format!("http://{h}"),
        h => format!("https://{h}"),
    };
    Ok(ImageRef {
        registry,
        repository,
        reference,
    })
}

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";

fn unavailable(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Unavailable(format!("registry {what}: {e}"))
}

/// Returns the manifest digest when the image is pullable with the given (optional `user:pass`)
/// credentials; `InvalidArgument` when the registry says it does not exist or refuses access.
pub async fn verify_image(
    http: &reqwest::Client,
    image: &str,
    auth: Option<&str>,
) -> Result<String> {
    let r = parse_image(image)?;
    let url = format!(
        "{}/v2/{}/manifests/{}",
        r.registry, r.repository, r.reference
    );
    let basic = auth.and_then(|a| a.split_once(':'));
    let send = |token: Option<String>| {
        let mut req = http.head(&url).header(reqwest::header::ACCEPT, ACCEPT);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        } else if let Some((u, p)) = basic {
            req = req.basic_auth(u, Some(p));
        }
        req.send()
    };
    let mut res = send(None)
        .await
        .map_err(|e| unavailable("unreachable", e))?;
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = res
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let Some(bearer) = challenge.strip_prefix("Bearer ") else {
            return Err(Error::InvalidArgument(format!(
                "image `{image}`: registry refused access (401) and offers no token auth"
            )));
        };
        let param = |k: &str| {
            bearer
                .split(',')
                .filter_map(|kv| kv.trim().split_once('='))
                .find(|(key, _)| key.trim() == k)
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
        };
        let Some(realm) = param("realm") else {
            return Err(Error::InvalidArgument(format!(
                "image `{image}`: registry token challenge without realm"
            )));
        };
        // the realm is whatever the registry answered: only an http(s) URL, and plaintext only
        // when the registry itself is reached over plaintext (credentials travel with it)
        let realm_url = reqwest::Url::parse(&realm).map_err(|_| {
            Error::InvalidArgument(format!("image `{image}`: bad token realm `{realm}`"))
        })?;
        let plain_ok =
            realm_url.scheme() == "http" && realm_url.host_str().is_some_and(plain_http_host);
        if !(realm_url.scheme() == "https" || plain_ok) {
            return Err(Error::InvalidArgument(format!(
                "image `{image}`: token realm must be https ({realm})"
            )));
        }
        let mut tok = http.get(realm_url).query(&[
            ("service", param("service").unwrap_or_default()),
            (
                "scope",
                param("scope").unwrap_or_else(|| format!("repository:{}:pull", r.repository)),
            ),
        ]);
        if let Some((u, p)) = basic {
            tok = tok.basic_auth(u, Some(p));
        }
        let tr = tok
            .send()
            .await
            .map_err(|e| unavailable("token endpoint", e))?;
        if tr.status() == reqwest::StatusCode::UNAUTHORIZED
            || tr.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(Error::InvalidArgument(format!(
                "image `{image}`: registry rejected the credentials"
            )));
        }
        let body: serde_json::Value = tr
            .error_for_status()
            .map_err(|e| unavailable("token endpoint", e))?
            .json()
            .await
            .map_err(|e| unavailable("token endpoint", e))?;
        let token = body["token"]
            .as_str()
            .or(body["access_token"].as_str())
            .ok_or_else(|| unavailable("token endpoint", "no token in response"))?
            .to_string();
        res = send(Some(token))
            .await
            .map_err(|e| unavailable("unreachable", e))?;
    }
    match res.status() {
        s if s.is_success() => {}
        reqwest::StatusCode::NOT_FOUND => {
            return Err(Error::InvalidArgument(format!(
                "image `{image}` not found in the registry"
            )))
        }
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            return Err(Error::InvalidArgument(format!(
                "image `{image}`: registry refused access ({})",
                res.status()
            )))
        }
        s => return Err(unavailable("manifest", s)),
    }
    let digest = res
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok(digest.unwrap_or_else(|| r.reference.clone()))
}

#[cfg(test)]
mod plain_tests {
    use super::plain_http_host;

    #[test]
    fn plaintext_registries_are_local_or_private() {
        assert!(plain_http_host("localhost") && plain_http_host("10.1.2.3"));
        assert!(plain_http_host("192.168.0.9"));
        assert!(!plain_http_host("ghcr.io") && !plain_http_host("100.64.0.1"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_refs() {
        let r = parse_image("nginx").unwrap();
        assert_eq!(r.registry, "https://registry-1.docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "latest");
        let r = parse_image("ghcr.io/acme/game-server:1.2.3").unwrap();
        assert_eq!(r.registry, "https://ghcr.io");
        assert_eq!(r.repository, "acme/game-server");
        assert_eq!(r.reference, "1.2.3");
        let r = parse_image("localhost:5000/g/s@sha256:abcd").unwrap();
        assert_eq!(r.registry, "http://localhost:5000");
        assert_eq!(r.repository, "g/s");
        assert_eq!(r.reference, "sha256:abcd");
        let r = parse_image("acme/server:v2").unwrap();
        assert_eq!(r.repository, "acme/server");
        assert!(parse_image("").is_err());
        assert!(parse_image("bad image").is_err());
    }
}
