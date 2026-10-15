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
