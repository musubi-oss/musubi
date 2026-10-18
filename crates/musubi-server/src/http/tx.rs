//! `POST /v1/server/tx` — several storage / wallet / inventory operations in one PostgreSQL
//! transaction: all of them land or none does. Storage CAS (`version`) and wallet balance
//! checks are enforced inside, so "take 100 gold, hand out the sword, mark the quest done" can
//! never half-apply.

use axum::{extract::State, routing::post, Json, Router};
use musubi_core::{AccountId, Error};
use musubi_storage::{Caller, WriteOp};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/server/tx", post(run))
}

pub const MAX_OPS: usize = 100;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Op {
    /// Storage write as the server, for `owner_id` (omitted = system-owned).
    StorageWrite {
        #[serde(default)]
        owner_id: Option<Uuid>,
        #[serde(flatten)]
        op: WriteOp,
    },
    WalletAdjust {
        account_id: Uuid,
        currency: String,
        delta: i64,
        #[serde(default = "default_reason")]
        reason: String,
        #[serde(default)]
        reference: String,
        #[serde(default)]
        idempotency_key: Option<String>,
        #[serde(default)]
        metadata: Value,
    },
    InventoryGrant {
        account_id: Uuid,
        item_id: String,
        #[serde(default = "one")]
        quantity: i64,
        #[serde(default)]
        instance: Value,
    },
    InventoryConsume {
        account_id: Uuid,
        entry_id: Uuid,
        #[serde(default = "one")]
        quantity: i64,
    },
}

fn default_reason() -> String {
    "server_tx".into()
}
fn one() -> i64 {
    1
}

#[derive(Deserialize)]
struct TxReq {
    ops: Vec<Op>,
}

async fn run(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<TxReq>,
) -> Result<Json<Value>, ApiError> {
    if r.ops.is_empty() || r.ops.len() > MAX_OPS {
        return Err(Error::InvalidArgument(format!("1..={MAX_OPS} ops required")).into());
    }
    // grant limits are checked up front against the pool (the tx must not hold them); recorded
    // after commit so a rolled-back tx does not eat the budget
    let mut grants: Vec<(AccountId, &str, String, i64)> = Vec::new();
    for op in &r.ops {
        match op {
            Op::WalletAdjust {
                account_id,
                currency,
                delta,
                ..
            } => grants.push((AccountId(*account_id), "currency", currency.clone(), *delta)),
            Op::InventoryGrant {
                account_id,
                item_id,
                quantity,
                ..
            } => grants.push((AccountId(*account_id), "item", item_id.clone(), *quantity)),
            _ => {}
        }
    }
    for (who, kind, key, amount) in &grants {
        crate::grant_limits::check(&s.db, *who, kind, key, *amount).await?;
    }

    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let mut results = Vec::with_capacity(r.ops.len());
    let mut pending = Vec::new();
    for op in r.ops {
        let out = match op {
            Op::StorageWrite { owner_id, op } => {
                let p = s
                    .storage
                    .write_tx(&mut tx, Caller::Server, owner_id, std::slice::from_ref(&op))
                    .await?;
                let ack = p.acks.first().cloned();
                pending.push(p);
                json!({ "type": "storage_write", "ack": ack })
            }
            Op::WalletAdjust {
                account_id,
                currency,
                delta,
                reason,
                reference,
                idempotency_key,
                metadata,
            } => {
                let (entry, replayed) = musubi_economy::EconomyService::adjust_tx(
                    &mut tx,
                    AccountId(account_id),
                    &currency,
                    delta,
                    &reason,
                    &reference,
                    idempotency_key.as_deref(),
                    if metadata.is_object() {
                        metadata
                    } else {
                        json!({})
                    },
                )
                .await?;
                json!({ "type": "wallet_adjust", "entry": entry, "replayed": replayed })
            }
            Op::InventoryGrant {
                account_id,
                item_id,
                quantity,
                instance,
            } => {
                let e = musubi_economy::EconomyService::grant_item_tx(
                    &mut tx,
                    AccountId(account_id),
                    &item_id,
                    quantity,
                    instance,
                )
                .await?;
                json!({ "type": "inventory_grant", "entry": e })
            }
            Op::InventoryConsume {
                account_id,
                entry_id,
                quantity,
            } => {
                let e = musubi_economy::EconomyService::consume_tx(
                    &mut tx,
                    AccountId(account_id),
                    entry_id,
                    quantity,
                )
                .await?;
                json!({ "type": "inventory_consume", "entry": e })
            }
        };
        results.push(out);
    }
    tx.commit().await.map_err(Error::internal)?;
    for p in pending {
        s.storage.commit_pending(p);
    }
    for (who, kind, key, amount) in &grants {
        crate::grant_limits::record(&s.db, *who, kind, key, *amount).await?;
    }
    Ok(Json(json!({ "results": results })))
}
