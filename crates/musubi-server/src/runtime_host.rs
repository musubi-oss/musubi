//! Server side of the runtime host API: what WASM guests reach through `musubi.host_call` /
//! `host.call`. Requests are JSON `{ "op": "...", ... }`; guests are trusted server code, so ops
//! act as the server (any owner / account), defaulting to the calling account where one applies.

use std::sync::Arc;

use musubi_core::{AccountId, Error, Result};
use musubi_runtime::{CallContext, HostApi};
use musubi_social::NewNotification;
use musubi_storage::{Caller, DeleteOp, ReadRef, WriteOp};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;

pub struct ServerHost {
    state: AppState,
    handle: tokio::runtime::Handle,
    /// Outbound client for `http.fetch`: no redirects (they could leave the allow-list).
    http: reqwest::Client,
    /// Token bucket for `http.fetch` (tokens, last refill).
    http_bucket: std::sync::Mutex<(f64, std::time::Instant)>,
}

/// `http.fetch` allow-list: exact host or `*.suffix` (any depth of subdomains, not the apex).
pub fn host_allowed(allow: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    allow.iter().any(|p| {
        let p = p.to_ascii_lowercase();
        match p.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
            None => host == p,
        }
    })
}

/// Deterministic numbers for `rng.random`: FNV-1a of the seed → SplitMix64.
pub fn seeded_values(seed: &str, count: usize, max: u64) -> Vec<u64> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut x = h;
    (0..count)
        .map(|_| {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)) % max.max(1)
        })
        .collect()
}

fn metric_name(s: &str) -> Result<String> {
    let ok = !s.is_empty()
        && s.len() <= 100
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit());
    if ok {
        Ok(format!("musubi_module_{s}"))
    } else {
        Err(Error::InvalidArgument(
            "metric name: [A-Za-z_][A-Za-z0-9_]*".into(),
        ))
    }
}

fn labels_of(req: &Value) -> Vec<metrics::Label> {
    req["labels"]
        .as_object()
        .map(|m| {
            m.iter()
                .take(8)
                .map(|(k, v)| {
                    metrics::Label::new(
                        k.clone(),
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// LiveOps answers per (account, op, names), good for `LIVEOPS_TTL`; shared by the module host
/// and the HTTP proxy so `POST /v1/server/liveops/changed` can drop them all at once.
pub type LiveopsCache =
    Arc<std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Value)>>>;

pub const LIVEOPS_CHANGED_TOPIC: &str = "liveops.changed";

/// A flag / live-event change arrived (webhook or bus): forget cached answers on this node and
/// tell modules (`hook_liveops_changed`). `broadcast` also informs the other nodes.
pub async fn liveops_changed(state: &AppState, payload: Value, broadcast: bool) {
    state.liveops_cache.lock().unwrap().clear();
    if broadcast {
        if let Some(c) = &state.cluster {
            if let Err(e) = c.publish(LIVEOPS_CHANGED_TOPIC, &payload).await {
                tracing::warn!(error = %e, "liveops change broadcast failed");
            }
        }
    }
    if state.runtime.has_hook("hook_liveops_changed") {
        let rt = state.runtime.clone();
        tokio::spawn(async move {
            rt.event(
                "liveops_changed",
                musubi_runtime::CallContext::default(),
                payload,
            )
            .await
        });
    }
}

/// Apply `liveops.changed` messages from other nodes.
pub fn start_liveops_sync(state: &AppState, cluster: &Arc<musubi_cluster::Cluster>) {
    let st = state.clone();
    let mut rx = cluster.subscribe();
    let self_id = cluster.node_id();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) if m.topic == LIVEOPS_CHANGED_TOPIC && m.origin != self_id => {
                    liveops_changed(&st, m.payload_value(), false).await;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}

/// How long a module sees the same flags / live events for one account.
pub const LIVEOPS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Install on the state's runtime; call once the state is complete.
pub fn install(state: &AppState) {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_millis(
            state.cfg.runtime.http_timeout_ms.max(100),
        ))
        .build()
        .unwrap_or_default();
    state.runtime.set_host(Arc::new(ServerHost {
        state: state.clone(),
        handle: tokio::runtime::Handle::current(),
        http,
        http_bucket: std::sync::Mutex::new((
            state.cfg.runtime.http_rate_per_sec as f64,
            std::time::Instant::now(),
        )),
    }));
}

fn str_field<'a>(req: &'a Value, name: &str) -> Result<&'a str> {
    req.get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::InvalidArgument(format!("host request needs string `{name}`")))
}

/// Optional integer field that must fit the column type (no silent wrap on `as` narrowing).
fn small_int<T: TryFrom<i64>>(req: &Value, name: &str, default: T) -> Result<T> {
    match req.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_i64()
            .and_then(|n| T::try_from(n).ok())
            .ok_or_else(|| Error::InvalidArgument(format!("`{name}` out of range"))),
    }
}

fn account(req: &Value, ctx: &CallContext) -> Result<AccountId> {
    match req.get("account_id").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s)
            .map(AccountId)
            .map_err(|_| Error::InvalidArgument("bad account_id".into())),
        None => ctx
            .account_id
            .ok_or_else(|| Error::InvalidArgument("no account_id and no calling account".into())),
    }
}

fn owner(req: &Value, ctx: &CallContext) -> Result<Uuid> {
    match req.get("owner_id").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s).map_err(|_| Error::InvalidArgument("bad owner_id".into())),
        None => Ok(ctx.account_id.map(|a| a.0).unwrap_or(Uuid::nil())),
    }
}

/// Session a stream op applies to: an explicit `session_id` (server context only) or the
/// calling player's own realtime session.
fn stream_session(req: &Value, ctx: &CallContext) -> Result<musubi_core::SessionId> {
    match req.get("session_id").and_then(|v| v.as_str()) {
        Some(sid) if ctx.server => crate::http::streams::parse_session(sid),
        Some(_) => Err(Error::PermissionDenied(
            "only server context may address another session".into(),
        )),
        None => ctx
            .session_id
            .ok_or_else(|| Error::InvalidArgument("no session_id and no calling session".into())),
    }
}

fn to_json<T: serde::Serialize>(v: T) -> Result<Value> {
    serde_json::to_value(v).map_err(Error::internal)
}

impl ServerHost {
    async fn dispatch(&self, ctx: &CallContext, req: Value) -> Result<Value> {
        let s = &self.state;
        let op = str_field(&req, "op")?;
        match op {
            "anticheat.flag" => {
                let account = musubi_core::AccountId(owner(&req, ctx)?);
                s.anticheat
                    .flag(
                        account,
                        None,
                        req.get("match_id").and_then(|v| v.as_str()),
                        "module",
                        str_field(&req, "kind")?,
                        small_int::<i32>(&req, "severity", 10)?,
                        req.get("details").cloned().unwrap_or(Value::Null),
                    )
                    .await
            }
            "stream.join" | "stream.leave" => {
                let stream = str_field(&req, "stream")?;
                crate::http::streams::check_stream_id(stream)?;
                let sid = stream_session(&req, ctx)?;
                let ok = if op == "stream.join" {
                    if !s.hub.join_stream(stream, sid) {
                        return Err(Error::NotFound(
                            "session is not connected to this node".into(),
                        ));
                    }
                    true
                } else {
                    s.hub.leave_stream(stream, sid)
                };
                Ok(json!({
                    "stream": stream,
                    (if op == "stream.join" { "joined" } else { "left" }): ok,
                    "count": s.hub.stream_count(stream),
                }))
            }
            "stream.send" => {
                let stream = str_field(&req, "stream")?;
                crate::http::streams::check_stream_id(stream)?;
                let data = req.get("data").cloned().unwrap_or(Value::Null);
                crate::http::streams::check_stream_data(&data)?;
                let sender = match ctx.session_id {
                    Some(sid) if !ctx.server => {
                        if !s.hub.in_stream(stream, sid) {
                            return Err(Error::PermissionDenied(
                                "caller is not a member of this stream".into(),
                            ));
                        }
                        s.hub.presence(sid)
                    }
                    _ => None,
                };
                let hide_sender = req.get("anonymous").and_then(|v| v.as_bool()) == Some(true);
                let delivered = s.hub.send_stream(
                    stream,
                    musubi_proto::ServerMessage::StreamData {
                        stream: stream.to_string(),
                        data,
                        sender: if hide_sender { None } else { sender },
                    },
                );
                Ok(json!({ "stream": stream, "delivered": delivered }))
            }
            "stream.count" => {
                let stream = str_field(&req, "stream")?;
                crate::http::streams::check_stream_id(stream)?;
                Ok(json!({ "stream": stream, "count": s.hub.stream_count(stream) }))
            }
            "stream.members" => {
                let stream = str_field(&req, "stream")?;
                crate::http::streams::check_stream_id(stream)?;
                if !ctx.server
                    && !ctx
                        .session_id
                        .is_some_and(|sid| s.hub.in_stream(stream, sid))
                {
                    return Err(Error::PermissionDenied(
                        "caller is not a member of this stream".into(),
                    ));
                }
                Ok(crate::http::streams::members_json(s, stream))
            }
            "stream.mine" => {
                let sid = stream_session(&req, ctx)?;
                Ok(json!({ "session_id": sid, "streams": s.hub.streams_of(sid) }))
            }
            "storage.read" => {
                let mut objs = s
                    .storage
                    .read(
                        Caller::Server,
                        &[ReadRef {
                            collection: str_field(&req, "collection")?.into(),
                            key: str_field(&req, "key")?.into(),
                            owner_id: Some(owner(&req, ctx)?),
                        }],
                    )
                    .await?;
                Ok(objs.pop().map(to_json).transpose()?.unwrap_or(Value::Null))
            }
            "storage.write" => {
                let acks = s
                    .storage
                    .write(
                        Caller::Server,
                        Some(owner(&req, ctx)?),
                        &[WriteOp {
                            merge: None,
                            base: None,
                            collection: str_field(&req, "collection")?.into(),
                            key: str_field(&req, "key")?.into(),
                            value: req.get("value").cloned().unwrap_or(Value::Null),
                            version: req
                                .get("version")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            read_permission: small_int::<i16>(&req, "read_permission", 1)?,
                            write_permission: small_int::<i16>(&req, "write_permission", 0)?,
                            ttl_secs: None,
                        }],
                    )
                    .await?;
                to_json(acks)
            }
            "storage.delete" => {
                s.storage
                    .delete(
                        Caller::Server,
                        Some(owner(&req, ctx)?),
                        &[DeleteOp {
                            collection: str_field(&req, "collection")?.into(),
                            key: str_field(&req, "key")?.into(),
                            version: None,
                        }],
                    )
                    .await?;
                Ok(json!({ "deleted": true }))
            }
            "account.get" => to_json(s.accounts.get(account(&req, ctx)?).await?),
            "wallet.get" => to_json(s.economy.wallet(account(&req, ctx)?).await?),
            "wallet.adjust" => {
                let delta = req["delta"]
                    .as_i64()
                    .ok_or_else(|| Error::InvalidArgument("wallet.adjust needs `delta`".into()))?;
                let who = account(&req, ctx)?;
                crate::grant_limits::check(
                    &s.db,
                    who,
                    "currency",
                    str_field(&req, "currency")?,
                    delta,
                )
                .await?;
                let (entry, replayed) = s
                    .economy
                    .adjust(
                        who,
                        str_field(&req, "currency")?,
                        delta,
                        req["reason"].as_str().unwrap_or("runtime"),
                        req["reference"].as_str().unwrap_or(""),
                        req["idempotency_key"].as_str(),
                        req.get("metadata").cloned().unwrap_or(json!({})),
                    )
                    .await?;
                if !replayed {
                    crate::grant_limits::record(
                        &s.db,
                        who,
                        "currency",
                        str_field(&req, "currency")?,
                        delta,
                    )
                    .await?;
                }
                Ok(json!({ "entry": to_json(entry)?, "replayed": replayed }))
            }
            "inventory.get" => to_json(s.economy.inventory(account(&req, ctx)?).await?),
            "inventory.grant" => {
                let who = account(&req, ctx)?;
                let item = str_field(&req, "item_id")?;
                let qty = req["quantity"].as_i64().unwrap_or(1);
                crate::grant_limits::check(&s.db, who, "item", item, qty).await?;
                let e = s
                    .economy
                    .grant_item(
                        who,
                        item,
                        qty,
                        req.get("instance").cloned().unwrap_or(json!({})),
                    )
                    .await?;
                crate::grant_limits::record(&s.db, who, "item", item, qty).await?;
                to_json(e)
            }
            "notify.send" => {
                s.notify
                    .send(vec![NewNotification {
                        user_id: account(&req, ctx)?,
                        sender_id: None,
                        subject: str_field(&req, "subject")?.into(),
                        content: req.get("content").cloned().unwrap_or(json!({})),
                        code: small_int::<i32>(&req, "code", 0)?,
                        persistent: req["persistent"].as_bool().unwrap_or(true),
                        ttl_secs: None,
                    }])
                    .await?;
                Ok(json!({ "sent": true }))
            }
            "liveops.flags" | "liveops.live_events" => {
                let op = str_field(&req, "op")?.to_string();
                let acc = account(&req, ctx)?;
                let names: Vec<String> = req["names"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let key = format!("{op}|{acc}|{}", names.join(","));
                let now = std::time::Instant::now();
                if let Some((at, v)) = self.state.liveops_cache.lock().unwrap().get(&key) {
                    if now.duration_since(*at) < LIVEOPS_TTL {
                        return Ok(v.clone());
                    }
                }
                let v = if op == "liveops.flags" {
                    crate::liveops::merged_flags(s, acc, &names).await?
                } else {
                    crate::liveops::merged_live_events(s, acc).await?
                };
                let mut cache = self.state.liveops_cache.lock().unwrap();
                cache.retain(|_, (at, _)| now.duration_since(*at) < LIVEOPS_TTL);
                cache.insert(key, (now, v.clone()));
                Ok(v)
            }
            "audiences.of" => {
                let acc = account(&req, ctx)?;
                let names = s.audiences.of(&s.db, &s.economy, acc).await?;
                Ok(json!({ "account_id": acc, "audiences": *names }))
            }
            "time.now" => {
                let now = chrono::Utc::now();
                Ok(
                    json!({ "unix": now.timestamp(), "unix_ms": now.timestamp_millis(), "iso": now.to_rfc3339() }),
                )
            }
            // `{seed?, count?, max?}`: with a seed the sequence is a pure function of the seed
            // (match modules pass e.g. "<match_id>:<tick>" for deterministic, replayable rolls)
            "rng.random" => {
                let count = req["count"].as_u64().unwrap_or(1).clamp(1, 1024) as usize;
                let max = req["max"].as_u64().unwrap_or(1 << 32).clamp(1, 1 << 53);
                let values: Vec<u64> = match &req["seed"] {
                    Value::Null => (0..count).map(|_| rand::random::<u64>() % max).collect(),
                    seed => {
                        let seed = seed
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| seed.to_string());
                        seeded_values(&seed, count, max)
                    }
                };
                Ok(json!({ "values": values, "max": max }))
            }
            "env.get" | "env.all" => crate::module_env::host_op(s, &req).await,
            "secrets.get" => {
                let name = str_field(&req, "name")?;
                let raw = s
                    .cfg
                    .runtime
                    .secrets
                    .get(name)
                    .ok_or_else(|| Error::NotFound(format!("secret `{name}`")))?;
                let value = match raw.strip_prefix("env:") {
                    Some(var) => std::env::var(var)
                        .map_err(|_| Error::NotFound(format!("secret `{name}`: {var} not set")))?,
                    None => raw.clone(),
                };
                Ok(json!({ "name": name, "value": value }))
            }
            "metrics.inc" | "metrics.gauge" => {
                let name = metric_name(str_field(&req, "name")?)?;
                let labels = labels_of(&req);
                let value = req["value"].as_f64().unwrap_or(1.0);
                if str_field(&req, "op")? == "metrics.inc" {
                    metrics::counter!(name, labels).increment(value.max(0.0) as u64);
                } else {
                    metrics::gauge!(name, labels).set(value);
                }
                Ok(json!({ "ok": true }))
            }
            "http.fetch" => {
                let url = reqwest::Url::parse(str_field(&req, "url")?)
                    .map_err(|e| Error::InvalidArgument(format!("http.fetch url: {e}")))?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(Error::InvalidArgument("http.fetch: http(s) only".into()));
                }
                let host = url.host_str().unwrap_or_default();
                if !host_allowed(&s.cfg.runtime.http_allow, host) {
                    return Err(Error::PermissionDenied(format!(
                        "http.fetch: host `{host}` is not in runtime.http_allow"
                    )));
                }
                {
                    let mut b = self.http_bucket.lock().unwrap_or_else(|e| e.into_inner());
                    let rate = s.cfg.runtime.http_rate_per_sec.max(1) as f64;
                    let now = std::time::Instant::now();
                    b.0 = (b.0 + now.duration_since(b.1).as_secs_f64() * rate).min(rate);
                    b.1 = now;
                    if b.0 < 1.0 {
                        return Err(Error::ResourceExhausted(
                            "http.fetch: outbound rate limit".into(),
                        ));
                    }
                    b.0 -= 1.0;
                }
                let method = req["method"].as_str().unwrap_or("GET").to_ascii_uppercase();
                let method = reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|_| Error::InvalidArgument("http.fetch: bad method".into()))?;
                let mut r = self.http.request(method, url);
                if let Some(h) = req["headers"].as_object() {
                    for (k, v) in h.iter().take(32) {
                        if let Some(v) = v.as_str() {
                            r = r.header(k.as_str(), v);
                        }
                    }
                }
                match &req["body"] {
                    Value::Null => {}
                    Value::String(b) => r = r.body(b.clone()),
                    other => r = r.json(other),
                }
                let max = s.cfg.runtime.http_max_body_kb.max(1) << 10;
                let res = r
                    .send()
                    .await
                    .map_err(|e| Error::Unavailable(format!("http.fetch: {e}")))?;
                let status = res.status().as_u16();
                let headers: serde_json::Map<String, Value> = res
                    .headers()
                    .iter()
                    .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), json!(v))))
                    .collect();
                if res.content_length().is_some_and(|l| l as usize > max) {
                    return Err(Error::ResourceExhausted(
                        "http.fetch: response too large".into(),
                    ));
                }
                let body = res
                    .bytes()
                    .await
                    .map_err(|e| Error::Unavailable(format!("http.fetch: {e}")))?;
                if body.len() > max {
                    return Err(Error::ResourceExhausted(
                        "http.fetch: response too large".into(),
                    ));
                }
                let text = String::from_utf8_lossy(&body).into_owned();
                let json = serde_json::from_slice::<Value>(&body).ok();
                Ok(json!({ "status": status, "headers": headers, "body": text, "json": json }))
            }
            "timer.schedule" => {
                let target = crate::http::timers::target_of(&req)?;
                let due = crate::http::timers::due_of(&req)?;
                crate::http::timers::schedule(
                    &s.db,
                    &target,
                    req.get("payload").cloned().unwrap_or(json!({})),
                    due,
                    req["key"].as_str(),
                )
                .await
            }
            "timer.cancel" => {
                let id = req["id"]
                    .as_str()
                    .map(|v| {
                        Uuid::parse_str(v)
                            .map_err(|_| Error::InvalidArgument("bad timer id".into()))
                    })
                    .transpose()?;
                Ok(
                    json!({ "cancelled": crate::http::timers::cancel(&s.db, id, req["key"].as_str()).await? }),
                )
            }
            other => Err(Error::InvalidArgument(format!("unknown host op `{other}`"))),
        }
    }
}

impl HostApi for ServerHost {
    fn call(&self, ctx: &CallContext, request: Value) -> Result<Value> {
        // Guests run on a blocking thread, so waiting on the async services here is fine.
        self.handle.block_on(self.dispatch(ctx, request))
    }
}

/// `musubi_fleet::ScaleHook` over the module runtime: a fleet's `policy.wasm` scaler calls an
/// export of a loaded module as the server, same JSON contract as the webhook scaler.
pub struct RuntimeScaleHook(pub Arc<musubi_runtime::Runtime>);

impl musubi_fleet::ScaleHook for RuntimeScaleHook {
    fn has(&self, module: &str, function: &str) -> bool {
        self.0.has_export(module, function)
    }

    fn call<'a>(
        &'a self,
        module: &'a str,
        function: &'a str,
        input: Value,
    ) -> musubi_fleet::BoxFut<'a, std::result::Result<Value, String>> {
        Box::pin(async move {
            let ctx = CallContext {
                server: true,
                ..Default::default()
            };
            let payload = serde_json::to_vec(&input).map_err(|e| e.to_string())?;
            let out = self
                .0
                .call_export(module, function, ctx, payload)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::from_slice(&out).map_err(|e| format!("non-JSON answer: {e}"))
        })
    }
}

#[cfg(test)]
mod narrowing_tests {
    use super::small_int;
    use serde_json::json;

    #[test]
    fn host_ints_must_fit_the_column() {
        let req = json!({ "a": 5, "big": 70_000, "neg": -1, "f": 1.5, "s": "7", "n": null });
        assert_eq!(small_int::<i16>(&req, "a", 0).unwrap(), 5);
        assert_eq!(small_int::<i16>(&req, "missing", 9).unwrap(), 9);
        assert_eq!(small_int::<i16>(&req, "n", 9).unwrap(), 9);
        assert_eq!(small_int::<i32>(&req, "big", 0).unwrap(), 70_000);
        assert!(small_int::<i16>(&req, "big", 0).is_err());
        assert!(small_int::<u16>(&req, "neg", 0).is_err());
        assert!(small_int::<i32>(&req, "f", 0).is_err());
        assert!(small_int::<i32>(&req, "s", 0).is_err());
    }
}
