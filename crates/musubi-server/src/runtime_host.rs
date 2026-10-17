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
                    liveops_changed(&st, m.payload, false).await;
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
    state.runtime.set_host(Arc::new(ServerHost {
        state: state.clone(),
        handle: tokio::runtime::Handle::current(),
    }));
}

fn str_field<'a>(req: &'a Value, name: &str) -> Result<&'a str> {
    req.get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::InvalidArgument(format!("host request needs string `{name}`")))
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

fn to_json<T: serde::Serialize>(v: T) -> Result<Value> {
    serde_json::to_value(v).map_err(Error::internal)
}

impl ServerHost {
    async fn dispatch(&self, ctx: &CallContext, req: Value) -> Result<Value> {
        let s = &self.state;
        match str_field(&req, "op")? {
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
                            collection: str_field(&req, "collection")?.into(),
                            key: str_field(&req, "key")?.into(),
                            value: req.get("value").cloned().unwrap_or(Value::Null),
                            version: req
                                .get("version")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            read_permission: req["read_permission"].as_i64().unwrap_or(1) as i16,
                            write_permission: req["write_permission"].as_i64().unwrap_or(0) as i16,
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
                let (entry, replayed) = s
                    .economy
                    .adjust(
                        account(&req, ctx)?,
                        str_field(&req, "currency")?,
                        delta,
                        req["reason"].as_str().unwrap_or("runtime"),
                        req["reference"].as_str().unwrap_or(""),
                        req["idempotency_key"].as_str(),
                        req.get("metadata").cloned().unwrap_or(json!({})),
                    )
                    .await?;
                Ok(json!({ "entry": to_json(entry)?, "replayed": replayed }))
            }
            "inventory.get" => to_json(s.economy.inventory(account(&req, ctx)?).await?),
            "inventory.grant" => to_json(
                s.economy
                    .grant_item(
                        account(&req, ctx)?,
                        str_field(&req, "item_id")?,
                        req["quantity"].as_i64().unwrap_or(1),
                        req.get("instance").cloned().unwrap_or(json!({})),
                    )
                    .await?,
            ),
            "notify.send" => {
                s.notify
                    .send(vec![NewNotification {
                        user_id: account(&req, ctx)?,
                        sender_id: None,
                        subject: str_field(&req, "subject")?.into(),
                        content: req.get("content").cloned().unwrap_or(json!({})),
                        code: req["code"].as_i64().unwrap_or(0) as i32,
                        persistent: req["persistent"].as_bool().unwrap_or(true),
                    }])
                    .await?;
                Ok(json!({ "sent": true }))
            }
            "liveops.flags" | "liveops.live_events" => {
                let op = str_field(&req, "op")?.to_string();
                let l = s
                    .liveops
                    .clone()
                    .ok_or_else(|| Error::NotFound("liveops (zanshin) not configured".into()))?;
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
                    l.flags(&acc.to_string(), &names).await?
                } else {
                    l.live_events(&acc.to_string()).await?
                };
                let mut cache = self.state.liveops_cache.lock().unwrap();
                cache.retain(|_, (at, _)| now.duration_since(*at) < LIVEOPS_TTL);
                cache.insert(key, (now, v.clone()));
                Ok(v)
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
