//! Validated store receipts → catalog grants. One receipt redeems once, for one account.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IapProduct {
    pub provider: String,
    pub product_id: String,
    pub store_item_id: String,
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
    }
}

const COLS: &str = "id, account_id, provider, transaction_id, product_id, store_item_id, environment, purchased_at_ms, created_at";

impl EconomyService {
    pub async fn upsert_iap_product(&self, p: IapProduct) -> Result<IapProduct> {
        if !matches!(p.provider.as_str(), "apple" | "google") {
            return Err(Error::InvalidArgument("provider: apple|google".into()));
        }
        if p.product_id.is_empty() || p.product_id.len() > 128 {
            return Err(Error::InvalidArgument("product_id".into()));
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
            "insert into musubi.iap_products (provider, product_id, store_item_id) values ($1, $2, $3)
             on conflict (provider, product_id) do update set store_item_id = excluded.store_item_id",
        )
        .bind(&p.provider)
        .bind(&p.product_id)
        .bind(&p.store_item_id)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(p)
    }

    pub async fn iap_products(&self) -> Result<Vec<IapProduct>> {
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("select provider, product_id, store_item_id from musubi.iap_products order by provider, product_id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| IapProduct {
                provider: r.0,
                product_id: r.1,
                store_item_id: r.2,
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
        let mapping: Option<(String,)> = sqlx::query_as(
            "select store_item_id from musubi.iap_products where provider = $1 and product_id = $2",
        )
        .bind(&input.provider)
        .bind(&input.product_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((store_item_id,)) = mapping else {
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
        })
    }
}
