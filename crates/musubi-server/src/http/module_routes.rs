//! Custom HTTP routes from module code (P1-36): export `http_<name>` of module `<module>` is
//! mounted at `/v1/x/{module}/{name}`. Per-route metadata (methods, auth mode, basic auth, CORS,
//! body limit, rate) comes from the IDL `[http."module/name"]` table; undeclared handlers behave
//! like RPCs (bearer or server key, any method).

use std::sync::OnceLock;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use dashmap::DashMap;
use musubi_core::Error;
use musubi_idl::{HttpAuth, HttpDef};
use musubi_runtime::CallContext;
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::http::ApiError;

const ALL_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";
const MAX_RESPONSE_HEADERS: usize = 32;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/x/{module}/{name}",
        get(handle)
            .post(handle)
            .put(handle)
            .patch(handle)
            .delete(handle)
            .options(handle),
    )
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn reply(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |x, (p, q)| x | (p ^ q)) == 0
}

struct Cors {
    origin: HeaderValue,
    wildcard: bool,
    credentials: bool,
    expose: Vec<String>,
}

fn cors_headers(c: &Cors, h: &mut HeaderMap) {
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        if c.wildcard && !c.credentials {
            HeaderValue::from_static("*")
        } else {
            c.origin.clone()
        },
    );
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    if c.credentials {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }
    if !c.expose.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&c.expose.join(", ")) {
            h.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, v);
        }
    }
}

fn rate_ok(key: &str, per_sec: f64) -> bool {
    static BUCKETS: OnceLock<DashMap<String, (f64, Instant)>> = OnceLock::new();
    let buckets = BUCKETS.get_or_init(DashMap::new);
    let burst = per_sec.max(1.0);
    let now = Instant::now();
    let mut e = buckets.entry(key.to_string()).or_insert((burst, now));
    let (tokens, at) = *e;
    let tokens = (tokens + at.elapsed().as_secs_f64() * per_sec).min(burst);
    if tokens >= 1.0 {
        *e = (tokens - 1.0, now);
        true
    } else {
        *e = (tokens, now);
        false
    }
}

fn parse_query(q: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        m.insert(
            crate::auth_providers::percent_decode(&k.replace('+', " ")),
            Value::String(crate::auth_providers::percent_decode(&v.replace('+', " "))),
        );
    }
    m
}

async fn handle(
    State(s): State<AppState>,
    Path((module, name)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !ident_ok(&module) || !ident_ok(&name) {
        return reply(StatusCode::NOT_FOUND, "no such module route");
    }
    let export = format!("http_{name}");
    if !s.runtime.has_export(&module, &export) {
        return reply(
            StatusCode::NOT_FOUND,
            format!("module `{module}` has no http handler `{name}`"),
        );
    }
    let def = s.idl.http_def(&module, &name).unwrap_or_default();
    let allowed_methods = if def.methods.is_empty() {
        ALL_METHODS.to_string()
    } else {
        let mut v = def.methods.clone();
        if !v.iter().any(|m| m == "OPTIONS") {
            v.push("OPTIONS".into());
        }
        v.join(", ")
    };

    // CORS: an Origin header against the declared allow-list.
    let mut cors: Option<Cors> = None;
    if let Some(c) = def.cors.as_ref().filter(|c| !c.origins.is_empty()) {
        if let Some(origin) = headers.get(header::ORIGIN) {
            let o = origin.to_str().unwrap_or("");
            let wildcard = c.origins.iter().any(|x| x == "*");
            if !wildcard && !c.origins.iter().any(|x| x == o) {
                return reply(StatusCode::FORBIDDEN, "origin not allowed");
            }
            cors = Some(Cors {
                origin: origin.clone(),
                wildcard,
                credentials: c.credentials,
                expose: c.expose_headers.clone(),
            });
            if method == Method::OPTIONS {
                let mut h = HeaderMap::new();
                cors_headers(cors.as_ref().unwrap(), &mut h);
                let methods = if c.methods.is_empty() {
                    allowed_methods.clone()
                } else {
                    c.methods.join(", ")
                };
                h.insert(
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_str(&methods).unwrap_or(HeaderValue::from_static("GET")),
                );
                let req_headers = if c.headers.is_empty() {
                    headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS).cloned()
                } else {
                    HeaderValue::from_str(&c.headers.join(", ")).ok()
                };
                if let Some(v) = req_headers {
                    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, v);
                }
                if let Some(age) = c.max_age_secs {
                    h.insert(
                        header::ACCESS_CONTROL_MAX_AGE,
                        HeaderValue::from_str(&age.to_string()).unwrap(),
                    );
                }
                return (StatusCode::NO_CONTENT, h).into_response();
            }
        }
    }
    if method == Method::OPTIONS && !def.methods.iter().any(|m| m == "OPTIONS") {
        let mut h = HeaderMap::new();
        h.insert(
            header::ALLOW,
            HeaderValue::from_str(&allowed_methods).unwrap(),
        );
        return (StatusCode::NO_CONTENT, h).into_response();
    }
    if !def.methods.is_empty() && !def.methods.iter().any(|m| m == method.as_str()) {
        let mut h = HeaderMap::new();
        h.insert(
            header::ALLOW,
            HeaderValue::from_str(&allowed_methods).unwrap(),
        );
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            h,
            Json(json!({ "error": format!("method {method} not allowed") })),
        )
            .into_response();
    }
    let max_body = def
        .max_body_bytes
        .unwrap_or(s.cfg.runtime.max_payload_kb << 10);
    if body.len() > max_body {
        return reply(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("body exceeds {max_body} bytes"),
        );
    }

    let (ctx, basic_user) = match auth(&s, &def, &headers).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };

    if let Some(rate) = def.rate_per_sec {
        let who = ctx
            .account_id
            .map(|a| a.to_string())
            .or_else(|| basic_user.clone())
            .or_else(|| {
                if ctx.server {
                    Some("server".into())
                } else {
                    headers
                        .get("x-forwarded-for")
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
                }
            })
            .unwrap_or_else(|| "anonymous".into());
        if !rate_ok(&format!("{module}/{name}:{who}"), rate) {
            return reply(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
        }
    }

    let mut hdrs = Map::new();
    for (k, v) in headers.iter() {
        if k == header::AUTHORIZATION || k == header::COOKIE || k.as_str() == "x-musubi-server-key"
        {
            continue;
        }
        if let Ok(v) = v.to_str() {
            hdrs.insert(k.as_str().to_string(), Value::String(v.to_string()));
        }
    }
    let body_text = std::str::from_utf8(&body).ok();
    let body_json = body_text.and_then(|t| serde_json::from_str::<Value>(t).ok());
    let envelope = json!({
        "module": module,
        "name": name,
        "method": method.as_str(),
        "path": uri.path(),
        "query": parse_query(uri.query().unwrap_or("")),
        "query_string": uri.query().unwrap_or(""),
        "headers": hdrs,
        "body": body_json,
        "body_text": body_text,
        "caller": {
            "account_id": ctx.account_id,
            "session_id": ctx.session_id,
            "username": ctx.username,
            "server": ctx.server,
            "basic_user": basic_user,
            "anonymous": !ctx.server && ctx.account_id.is_none() && basic_user.is_none(),
        },
    });
    let payload = serde_json::to_vec(&envelope).unwrap_or_default();
    let out = match s.runtime.call_export(&module, &export, ctx, payload).await {
        Ok(o) => o,
        Err(e) => {
            let mut r = ApiError::from(e).into_response();
            if let Some(c) = &cors {
                cors_headers(c, r.headers_mut());
            }
            return r;
        }
    };
    let mut resp = render(out);
    if let Some(c) = &cors {
        cors_headers(c, resp.headers_mut());
    }
    resp
}

/// Resolve the caller per the declared auth mode. `Err` carries the ready 401/403 response.
async fn auth(
    s: &AppState,
    def: &HttpDef,
    headers: &HeaderMap,
) -> Result<(CallContext, Option<String>), Box<Response>> {
    match def.auth {
        HttpAuth::None => Ok((CallContext::default(), None)),
        HttpAuth::Basic => {
            let b = def.basic.clone().unwrap_or_default();
            let realm = b.realm.clone().unwrap_or_else(|| "musubi".into());
            let challenge = move || {
                let mut r = reply(StatusCode::UNAUTHORIZED, "basic auth required");
                r.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_str(&format!("Basic realm=\"{realm}\""))
                        .unwrap_or(HeaderValue::from_static("Basic")),
                );
                r
            };
            let Some(raw) = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Basic "))
            else {
                return Err(Box::new(challenge()));
            };
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(raw.trim())
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            let (user, pass) = decoded.split_once(':').unwrap_or((&decoded, ""));
            let ok = b
                .users
                .get(user)
                .is_some_and(|expected| ct_eq(expected, pass));
            if !ok {
                return Err(Box::new(challenge()));
            }
            let mut ctx = CallContext::default();
            ctx.vars
                .insert("basic_user".into(), Value::String(user.to_string()));
            Ok((ctx, Some(user.to_string())))
        }
        HttpAuth::Any | HttpAuth::Player | HttpAuth::Server => {
            let ctx = crate::http::rpc::context(s, headers)
                .await
                .map_err(|e| Box::new(e.into_response()))?;
            if def.auth == HttpAuth::Server && !ctx.server {
                return Err(Box::new(
                    ApiError::from(Error::PermissionDenied("route is server-only".into()))
                        .into_response(),
                ));
            }
            if def.auth == HttpAuth::Player && ctx.server {
                return Err(Box::new(
                    ApiError::from(Error::PermissionDenied("route is player-only".into()))
                        .into_response(),
                ));
            }
            Ok((ctx, None))
        }
    }
}

/// Module output → HTTP response. A JSON object with a numeric `status` is a response envelope
/// `{status, headers?, body?}` (string body = text, other = JSON); anything else is the body.
fn render(out: Vec<u8>) -> Response {
    let parsed: Option<Value> = serde_json::from_slice(&out).ok();
    let envelope = parsed.as_ref().and_then(|v| {
        let o = v.as_object()?;
        let status = o.get("status")?.as_u64()?;
        Some((o, status))
    });
    let Some((o, status)) = envelope else {
        let ct = if parsed.is_some() {
            "application/json"
        } else if std::str::from_utf8(&out).is_ok() {
            "text/plain; charset=utf-8"
        } else {
            "application/octet-stream"
        };
        return (StatusCode::OK, [(header::CONTENT_TYPE, ct)], out).into_response();
    };
    let status = StatusCode::from_u16(status as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut h = HeaderMap::new();
    if let Some(hs) = o.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in hs.iter().take(MAX_RESPONSE_HEADERS) {
            let lk = k.to_ascii_lowercase();
            if lk == "content-length" || lk == "transfer-encoding" || lk == "connection" {
                continue;
            }
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(lk.as_bytes()),
                HeaderValue::from_str(&val),
            ) {
                h.insert(n, v);
            }
        }
    }
    let body: Vec<u8> = match o.get("body") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(t)) => {
            if !h.contains_key(header::CONTENT_TYPE) {
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
            }
            t.clone().into_bytes()
        }
        Some(v) => {
            if !h.contains_key(header::CONTENT_TYPE) {
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            serde_json::to_vec(v).unwrap_or_default()
        }
    };
    (status, h, body).into_response()
}
