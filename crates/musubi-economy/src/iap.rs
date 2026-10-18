//! Validated store receipts → catalog grants. One receipt redeems once, for one account.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IapProduct {
    pub provider: String,
    pub product_id: String,
    pub store_item_id: String,
    /// Steam only: price charged by InitTxn, in minor units of the order currency.
    #[serde(default)]
    pub amount_cents: i64,
    #[serde(default)]
    pub description: String,
    /// `consumable` (default) or `subscription` (auto-renewing: one `iap_subscriptions` row per
    /// original transaction, the store item granted on every renewal).
    #[serde(default)]
    pub kind: String,
    /// Subscriptions: seconds of access kept after the period ends without a renewal.
    #[serde(default)]
    pub grace_secs: i64,
}

/// Where a subscription stands at `now_ms` given its period end and grace.
pub fn subscription_status(expires_at_ms: i64, grace_secs: i64, now_ms: i64) -> &'static str {
    if now_ms < expires_at_ms {
        "active"
    } else if now_ms < expires_at_ms.saturating_add(grace_secs.saturating_mul(1000)) {
        "grace"
    } else {
        "expired"
    }
}

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// A provider-validated transaction, ready to be redeemed.
#[derive(Debug, Clone)]
pub struct ReceiptInput {
    pub provider: String,
    pub transaction_id: String,
    pub product_id: String,
    pub environment: String,
    pub purchased_at_ms: i64,
    pub quantity: i64,
    pub original_transaction_id: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub id: Uuid,
    pub provider: String,
    pub transaction_id: String,
    pub product_id: String,
    pub store_item_id: String,
    pub item_id: Option<String>,
    pub quantity: i64,
    pub environment: String,
    pub purchased_at_ms: i64,
    pub created_at: OffsetDateTime,
    pub replayed: bool,
    pub revoked: bool,
}

type ReceiptRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    String,
    String,
    i64,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

fn receipt(r: ReceiptRow, replayed: bool) -> Receipt {
    Receipt {
        id: r.0,
        provider: r.2,
        transaction_id: r.3,
        product_id: r.4,
        store_item_id: r.5,
        item_id: None,
        quantity: 0,
        environment: r.6,
        purchased_at_ms: r.7,
        created_at: r.8,
        replayed,
        revoked: r.9.is_some(),
    }
}

const COLS: &str = "id, account_id, provider, transaction_id, product_id, store_item_id, environment, purchased_at_ms, created_at, revoked_at";

impl EconomyService {
    pub async fn upsert_iap_product(&self, p: IapProduct) -> Result<IapProduct> {
        if !matches!(
            p.provider.as_str(),
            "apple"
                | "google"
                | "steam"
                | "psn"
                | "xbox"
                | "nintendo"
                | "epic"
                | "huawei"
                | "samsung"
        ) {
            return Err(Error::InvalidArgument(
                "provider: apple|google|steam|psn|xbox|nintendo|epic|huawei|samsung".into(),
            ));
        }
        if p.product_id.is_empty() || p.product_id.len() > 128 {
            return Err(Error::InvalidArgument("product_id".into()));
        }
        let mut p = p;
        if p.kind.is_empty() {
            p.kind = "consumable".into();
        }
        if !matches!(p.kind.as_str(), "consumable" | "subscription") {
            return Err(Error::InvalidArgument(
                "kind: consumable|subscription".into(),
            ));
        }
        if p.grace_secs < 0 || p.grace_secs > 30 * 86_400 {
            return Err(Error::InvalidArgument("grace_secs: 0..=30 days".into()));
        }
        let exists: Option<(String,)> =
            sqlx::query_as("select id from musubi.store_items where id = $1")
                .bind(&p.store_item_id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        if exists.is_none() {
            return Err(Error::NotFound(format!("store item {}", p.store_item_id)));
        }
        sqlx::query(
            "insert into musubi.iap_products (provider, product_id, store_item_id, amount_cents, description, kind, grace_secs) values ($1, $2, $3, $4, $5, $6, $7)
             on conflict (provider, product_id) do update set store_item_id = excluded.store_item_id, amount_cents = excluded.amount_cents, description = excluded.description, kind = excluded.kind, grace_secs = excluded.grace_secs",
        )
        .bind(&p.provider)
        .bind(&p.product_id)
        .bind(&p.store_item_id)
        .bind(p.amount_cents)
        .bind(&p.description)
        .bind(&p.kind)
        .bind(p.grace_secs)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(p)
    }

    pub async fn iap_products(&self) -> Result<Vec<IapProduct>> {
        let rows: Vec<(String, String, String, String, i64)> =
            sqlx::query_as("select provider, product_id, store_item_id, kind, grace_secs from musubi.iap_products order by provider, product_id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| IapProduct {
                provider: r.0,
                product_id: r.1,
                store_item_id: r.2,
                amount_cents: 0,
                description: String::new(),
                kind: r.3,
                grace_secs: r.4,
            })
            .collect())
    }

    pub async fn receipts(&self, account: AccountId, limit: i64) -> Result<Vec<Receipt>> {
        let rows: Vec<ReceiptRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.iap_receipts where account_id = $1 order by created_at desc limit $2"
        ))
        .bind(account.0)
        .bind(limit.clamp(1, crate::MAX_LEDGER_PAGE))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|r| receipt(r, true)).collect())
    }

    /// Grants the mapped store item for a validated transaction. Replaying the same transaction
    /// from the same account returns the original receipt; from another account it is a conflict.
    pub async fn redeem(&self, account: AccountId, input: ReceiptInput) -> Result<Receipt> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let prior: Option<ReceiptRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.iap_receipts where provider = $1 and transaction_id = $2 for update"
        ))
        .bind(&input.provider)
        .bind(&input.transaction_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if let Some(p) = prior {
            if p.1 != account.0 {
                return Err(Error::Conflict(
                    "receipt already redeemed by another account".into(),
                ));
            }
            return Ok(receipt(p, true));
        }
        let mapping: Option<(String, String, i64)> = sqlx::query_as(
            "select store_item_id, kind, grace_secs from musubi.iap_products where provider = $1 and product_id = $2",
        )
        .bind(&input.provider)
        .bind(&input.product_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((store_item_id, kind, grace_secs)) = mapping else {
            return Err(Error::NotFound(format!(
                "iap product {}:{} is not mapped",
                input.provider, input.product_id
            )));
        };
        let item: Option<(String, i64)> =
            sqlx::query_as("select item_id, quantity from musubi.store_items where id = $1")
                .bind(&store_item_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some((item_id, qty)) = item else {
            return Err(Error::NotFound(format!("store item {store_item_id}")));
        };
        let quantity = qty.saturating_mul(input.quantity.max(1));
        Self::grant_item_tx(
            &mut tx,
            account,
            &item_id,
            quantity,
            Value::Object(Default::default()),
        )
        .await?;
        if kind == "subscription" {
            let orig = input
                .original_transaction_id
                .clone()
                .unwrap_or_else(|| input.transaction_id.clone());
            let expires = input.expires_at_ms.unwrap_or(0);
            let status = subscription_status(expires, grace_secs, now_ms());
            sqlx::query(
                "insert into musubi.iap_subscriptions (id, account_id, provider, product_id, store_item_id, original_transaction_id, latest_transaction_id, status, started_at_ms, expires_at_ms, environment, raw)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                 on conflict (provider, original_transaction_id) do update set
                   latest_transaction_id = excluded.latest_transaction_id, product_id = excluded.product_id,
                   status = case when musubi.iap_subscriptions.status = 'revoked' then 'revoked' else excluded.status end,
                   expires_at_ms = greatest(musubi.iap_subscriptions.expires_at_ms, excluded.expires_at_ms),
                   raw = excluded.raw, updated_at = now()",
            )
            .bind(Uuid::now_v7())
            .bind(account.0)
            .bind(&input.provider)
            .bind(&input.product_id)
            .bind(&store_item_id)
            .bind(&orig)
            .bind(&input.transaction_id)
            .bind(status)
            .bind(input.purchased_at_ms)
            .bind(expires)
            .bind(&input.environment)
            .bind(&input.raw)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let id = Uuid::now_v7();
        let created_at = OffsetDateTime::now_utc();
        sqlx::query(
            "insert into musubi.iap_receipts (id, account_id, provider, transaction_id, product_id, store_item_id, environment, purchased_at_ms, raw, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(account.0)
        .bind(&input.provider)
        .bind(&input.transaction_id)
        .bind(&input.product_id)
        .bind(&store_item_id)
        .bind(&input.environment)
        .bind(input.purchased_at_ms)
        .bind(&input.raw)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Receipt {
            id,
            provider: input.provider,
            transaction_id: input.transaction_id,
            product_id: input.product_id,
            store_item_id,
            item_id: Some(item_id),
            quantity,
            environment: input.environment,
            purchased_at_ms: input.purchased_at_ms,
            created_at,
            replayed: false,
            revoked: false,
        })
    }

    /// Store-side refund/revocation: mark the receipt and take back the granted items (clamped at
    /// what the player still holds). `reference` is the provider transaction id or purchase token.
    /// A product's mapping, if any (`kind` tells a verify handler which store API to call).
    pub async fn iap_product(
        &self,
        provider: &str,
        product_id: &str,
    ) -> Result<Option<IapProduct>> {
        let row: Option<(String, String, String, i64)> = sqlx::query_as(
            "select store_item_id, kind, description, grace_secs from musubi.iap_products where provider = $1 and product_id = $2",
        )
        .bind(provider)
        .bind(product_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(
            |(store_item_id, kind, description, grace_secs)| IapProduct {
                provider: provider.into(),
                product_id: product_id.into(),
                store_item_id,
                amount_cents: 0,
                description,
                kind,
                grace_secs,
            },
        ))
    }

    /// An account's subscriptions with their state as of now (`active`, `grace`, `expired`,
    /// `revoked`); `active: true` while the player keeps access (active or grace).
    pub async fn subscriptions_of(&self, account: AccountId) -> Result<Vec<Value>> {
        type Row = (
            Uuid,
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            i64,
            bool,
            String,
            i64,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select s.id, s.provider, s.product_id, s.store_item_id, s.original_transaction_id, s.latest_transaction_id, s.status,
                    s.started_at_ms, s.expires_at_ms, s.auto_renew, s.environment, coalesce(p.grace_secs, 0)
             from musubi.iap_subscriptions s
             left join musubi.iap_products p on p.provider = s.provider and p.product_id = s.product_id
             where s.account_id = $1 order by s.started_at_ms desc",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let now = now_ms();
        Ok(rows
            .into_iter()
            .map(|r| {
                let status = if r.6 == "revoked" {
                    "revoked"
                } else {
                    subscription_status(r.8, r.11, now)
                };
                serde_json::json!({
                    "id": r.0, "provider": r.1, "product_id": r.2, "store_item_id": r.3,
                    "original_transaction_id": r.4, "latest_transaction_id": r.5,
                    "status": status, "active": matches!(status, "active" | "grace"),
                    "started_at_ms": r.7, "expires_at_ms": r.8, "auto_renew": r.9,
                    "environment": r.10, "grace_secs": r.11,
                })
            })
            .collect())
    }

    /// Move subscriptions whose period ended into `grace` (while the product's grace lasts) and
    /// then `expired`, once each; returns the transitions for events. Store notifications usually
    /// arrive first — this catches the ones that never come.
    pub async fn sweep_subscriptions(&self) -> Result<Vec<Value>> {
        let now = now_ms();
        type Row = (Uuid, Uuid, String, String, String, i64, String);
        let mut out = Vec::new();
        for (from, to, cond) in [
            (
                "active",
                "grace",
                "s.expires_at_ms <= $1 and s.expires_at_ms + coalesce(p.grace_secs, 0)::bigint * 1000 > $1",
            ),
            (
                "active",
                "expired",
                "s.expires_at_ms + coalesce(p.grace_secs, 0)::bigint * 1000 <= $1",
            ),
            (
                "grace",
                "expired",
                "s.expires_at_ms + coalesce(p.grace_secs, 0)::bigint * 1000 <= $1",
            ),
        ] {
            let rows: Vec<Row> = sqlx::query_as(&format!(
                "update musubi.iap_subscriptions s set status = '{to}', updated_at = now()
                 from musubi.iap_products p
                 where p.provider = s.provider and p.product_id = s.product_id
                   and s.status = '{from}' and {cond}
                 returning s.id, s.account_id, s.provider, s.product_id, s.store_item_id, s.expires_at_ms, s.status"
            ))
            .bind(now)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
            for r in rows {
                out.push(serde_json::json!({
                    "id": r.0, "account_id": r.1, "provider": r.2, "product_id": r.3,
                    "store_item_id": r.4, "expires_at_ms": r.5, "status": r.6, "from": from,
                }));
            }
        }
        Ok(out)
    }

    /// A store notification about a subscription: new period end, renewal preference, or its end.
    pub async fn update_subscription(
        &self,
        provider: &str,
        original_transaction_id: &str,
        latest_transaction_id: Option<&str>,
        expires_at_ms: Option<i64>,
        status: Option<&str>,
        auto_renew: Option<bool>,
    ) -> Result<Option<Value>> {
        let row: Option<(Uuid, String, String, i64)> = sqlx::query_as(
            "update musubi.iap_subscriptions set
               latest_transaction_id = coalesce($3, latest_transaction_id),
               expires_at_ms = coalesce($4, expires_at_ms),
               status = coalesce($5, status),
               auto_renew = coalesce($6, auto_renew),
               updated_at = now()
             where provider = $1 and original_transaction_id = $2
             returning account_id, product_id, status, expires_at_ms",
        )
        .bind(provider)
        .bind(original_transaction_id)
        .bind(latest_transaction_id)
        .bind(expires_at_ms)
        .bind(status)
        .bind(auto_renew)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(|(account_id, product_id, status, expires_at_ms)| {
            serde_json::json!({"account_id": account_id, "provider": provider, "product_id": product_id, "status": status, "expires_at_ms": expires_at_ms})
        }))
    }

    pub async fn revoke_receipt(
        &self,
        provider: &str,
        reference: &str,
        reason: &str,
    ) -> Result<Option<Receipt>> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<ReceiptRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.iap_receipts
             where provider = $1 and (transaction_id = $2 or raw->>'purchaseToken' = $2) for update"
        ))
        .bind(provider)
        .bind(reference)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(r) = row else { return Ok(None) };
        let (units,): (i64,) = sqlx::query_as(
            "select coalesce((raw->>'quantity')::bigint, 1) from musubi.iap_receipts where id = $1",
        )
        .bind(r.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if r.9.is_some() {
            return Ok(Some(receipt(r, true)));
        }
        let account = r.1;
        let store: Option<(String, i64)> =
            sqlx::query_as("select item_id, quantity from musubi.store_items where id = $1")
                .bind(&r.5)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if let Some((item_id, per_unit)) = store {
            let mut left = per_unit.max(1) * units.max(1);
            let rows: Vec<(Uuid, i64)> = sqlx::query_as(
                "select id, quantity from musubi.inventory where account_id = $1 and item_id = $2 order by quantity desc for update",
            )
            .bind(account)
            .bind(&item_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(Error::internal)?;
            for (id, have) in rows {
                if left <= 0 {
                    break;
                }
                let take = have.min(left);
                left -= take;
                if take >= have {
                    sqlx::query("delete from musubi.inventory where id = $1").bind(id)
                } else {
                    sqlx::query("update musubi.inventory set quantity = quantity - $2, updated_at = now() where id = $1")
                        .bind(id)
                        .bind(take)
                }
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            }
        }
        let row: ReceiptRow = sqlx::query_as(&format!(
            "update musubi.iap_receipts set revoked_at = now(), raw = raw || jsonb_build_object('revocation_reason', $2::text) where id = $1 returning {COLS}"
        ))
        .bind(r.0)
        .bind(reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Some(receipt(row, false)))
    }

    // ----- steam orders -----

    pub async fn steam_product(&self, store_item_id: &str) -> Result<Option<IapProduct>> {
        let row: Option<(String, String, String, i64, String)> = sqlx::query_as(
            "select provider, product_id, store_item_id, amount_cents, description from musubi.iap_products where provider = 'steam' and store_item_id = $1",
        )
        .bind(store_item_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(|r| IapProduct {
            provider: r.0,
            product_id: r.1,
            store_item_id: r.2,
            amount_cents: r.3,
            description: r.4,
            kind: "consumable".into(),
            grace_secs: 0,
        }))
    }

    /// Records a pending Steam order and returns its 63-bit order id.
    pub async fn create_steam_order(
        &self,
        account: AccountId,
        product: &IapProduct,
        quantity: i64,
        steam_id: &str,
        currency: &str,
    ) -> Result<i64> {
        let order_id = (u64::from_be_bytes(
            sha2::Sha256::digest(Uuid::now_v7().as_bytes())[..8]
                .try_into()
                .expect("8 bytes"),
        ) >> 1) as i64;
        sqlx::query(
            "insert into musubi.iap_orders (order_id, account_id, store_item_id, product_id, quantity, steam_id, currency) values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(order_id)
        .bind(account.0)
        .bind(&product.store_item_id)
        .bind(&product.product_id)
        .bind(quantity)
        .bind(steam_id)
        .bind(currency)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(order_id)
    }

    pub async fn steam_order(&self, order_id: i64) -> Result<Option<SteamOrder>> {
        let row: Option<SteamOrderRow> = sqlx::query_as(
            "select account_id, store_item_id, product_id, quantity, steam_id, currency, finalized_at from musubi.iap_orders where order_id = $1",
        )
        .bind(order_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(|r| SteamOrder {
            order_id,
            account_id: AccountId(r.0),
            store_item_id: r.1,
            product_id: r.2,
            quantity: r.3,
            steam_id: r.4,
            currency: r.5,
            finalized: r.6.is_some(),
        }))
    }

    pub async fn finalize_steam_order(&self, order_id: i64) -> Result<()> {
        sqlx::query("update musubi.iap_orders set finalized_at = now() where order_id = $1")
            .bind(order_id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Live (not revoked) Steam receipts as `(transaction_id, order_id)` for refund reconciliation.
    pub async fn steam_receipt_orders(&self) -> Result<Vec<(String, i64)>> {
        sqlx::query_as(
            "select transaction_id, (raw->>'orderid')::bigint from musubi.iap_receipts where provider = 'steam' and revoked_at is null and raw->>'orderid' is not null",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)
    }
}

type SteamOrderRow = (
    Uuid,
    String,
    String,
    i64,
    String,
    String,
    Option<OffsetDateTime>,
);

#[derive(Debug, Clone, Serialize)]
pub struct SteamOrder {
    pub order_id: i64,
    pub account_id: AccountId,
    pub store_item_id: String,
    pub product_id: String,
    pub quantity: i64,
    pub steam_id: String,
    pub currency: String,
    pub finalized: bool,
}
