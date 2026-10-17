//! Group inventory (plan 5.4): a shared item store per group with its own ledger. Members put
//! stackable items in from their own inventory; officers with the right role bit hand items out
//! to a member. Both legs run in one transaction.

use musubi_core::{AccountId, Error, Result};
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::EconomyService;

#[derive(Debug, Clone, Serialize)]
pub struct GroupItem {
    pub item_id: String,
    pub quantity: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupInventoryEntry {
    pub id: Uuid,
    pub account_id: Option<AccountId>,
    pub item_id: String,
    pub delta: i64,
    pub quantity_after: i64,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type LedgerRow = (Uuid, Option<Uuid>, String, i64, i64, String, OffsetDateTime);

impl EconomyService {
    pub async fn group_inventory(&self, group: Uuid) -> Result<Vec<GroupItem>> {
        let rows: Vec<(String, i64, OffsetDateTime)> = sqlx::query_as(
            "select item_id, quantity, updated_at from musubi.group_inventory where group_id = $1 order by item_id",
        )
        .bind(group)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(item_id, quantity, updated_at)| GroupItem {
                item_id,
                quantity,
                updated_at,
            })
            .collect())
    }

    pub async fn group_inventory_ledger(
        &self,
        group: Uuid,
        limit: i64,
    ) -> Result<Vec<GroupInventoryEntry>> {
        let rows: Vec<LedgerRow> = sqlx::query_as(
            "select id, account_id, item_id, delta, quantity_after, reason, created_at
             from musubi.group_inventory_ledger where group_id = $1
             order by created_at desc, id desc limit $2",
        )
        .bind(group)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(ledger_entry).collect())
    }

    /// Move `quantity` of the member's inventory entry `entry` into the group store. Only
    /// stackable items can be pooled: the store counts items per id and keeps no per-instance data.
    pub async fn group_inventory_deposit(
        &self,
        group: Uuid,
        account: AccountId,
        entry: Uuid,
        quantity: i64,
        reason: &str,
    ) -> Result<GroupInventoryEntry> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(String, i64, bool)> = sqlx::query_as(
            "select i.item_id, i.quantity, it.stackable from musubi.inventory i
             join musubi.items it on it.id = i.item_id
             where i.id = $1 and i.account_id = $2 for update of i",
        )
        .bind(entry)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, have, stackable)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        if !stackable {
            return Err(Error::InvalidArgument(
                "only stackable items can be pooled in a group inventory".into(),
            ));
        }
        if have < quantity {
            return Err(Error::ResourceExhausted(format!(
                "have {have} of {item_id}, depositing {quantity}"
            )));
        }
        if have == quantity {
            sqlx::query("delete from musubi.inventory where id = $1")
                .bind(entry)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        } else {
            sqlx::query(
                "update musubi.inventory set quantity = quantity - $2, updated_at = now() where id = $1",
            )
            .bind(entry)
            .bind(quantity)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let (after,): (i64,) = sqlx::query_as(
            "insert into musubi.group_inventory (group_id, item_id, quantity) values ($1, $2, $3)
             on conflict (group_id, item_id) do update
             set quantity = musubi.group_inventory.quantity + $3, updated_at = now()
             returning quantity",
        )
        .bind(group)
        .bind(&item_id)
        .bind(quantity)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let e = ledger_insert(
            &mut tx,
            group,
            Some(account),
            &item_id,
            quantity,
            after,
            reason,
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(e)
    }

    /// Hand `quantity` of `item_id` from the group store to `account`'s inventory.
    pub async fn group_inventory_withdraw(
        &self,
        group: Uuid,
        account: AccountId,
        item_id: &str,
        quantity: i64,
        reason: &str,
    ) -> Result<GroupInventoryEntry> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let have: Option<(i64,)> = sqlx::query_as(
            "select quantity from musubi.group_inventory where group_id = $1 and item_id = $2 for update",
        )
        .bind(group)
        .bind(item_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let have = have.map(|h| h.0).unwrap_or(0);
        if have < quantity {
            return Err(Error::ResourceExhausted(format!(
                "group has {have} of {item_id}, withdrawing {quantity}"
            )));
        }
        let after = have - quantity;
        if after == 0 {
            sqlx::query("delete from musubi.group_inventory where group_id = $1 and item_id = $2")
                .bind(group)
                .bind(item_id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        } else {
            sqlx::query(
                "update musubi.group_inventory set quantity = $3, updated_at = now()
                 where group_id = $1 and item_id = $2",
            )
            .bind(group)
            .bind(item_id)
            .bind(after)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        Self::grant_item_tx(&mut tx, account, item_id, quantity, json!({})).await?;
        let e = ledger_insert(
            &mut tx,
            group,
            Some(account),
            item_id,
            -quantity,
            after,
            reason,
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(e)
    }
}

async fn ledger_insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group: Uuid,
    account: Option<AccountId>,
    item_id: &str,
    delta: i64,
    after: i64,
    reason: &str,
) -> Result<GroupInventoryEntry> {
    let row: LedgerRow = sqlx::query_as(
        "insert into musubi.group_inventory_ledger (id, group_id, account_id, item_id, delta, quantity_after, reason)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, account_id, item_id, delta, quantity_after, reason, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(group)
    .bind(account.map(|a| a.0))
    .bind(item_id)
    .bind(delta)
    .bind(after)
    .bind(reason)
    .fetch_one(&mut **tx)
    .await
    .map_err(Error::internal)?;
    Ok(ledger_entry(row))
}

fn ledger_entry(r: LedgerRow) -> GroupInventoryEntry {
    GroupInventoryEntry {
        id: r.0,
        account_id: r.1.map(AccountId),
        item_id: r.2,
        delta: r.3,
        quantity_after: r.4,
        reason: r.5,
        created_at: r.6,
    }
}
