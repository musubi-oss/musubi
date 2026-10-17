//! Group treasury (plan 5.4): a wallet per group with its own ledger. Members pay in from their
//! own wallet; officers with the right role bit pay out to a member. Both legs run in one
//! transaction so the money is never in two places or nowhere.

use musubi_core::{AccountId, Error, Result};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Balance, EconomyService};

#[derive(Debug, Clone, Serialize)]
pub struct TreasuryEntry {
    pub id: Uuid,
    pub account_id: Option<AccountId>,
    pub currency: String,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type TreasuryRow = (Uuid, Option<Uuid>, String, i64, i64, String, OffsetDateTime);

impl EconomyService {
    pub async fn treasury(&self, group: Uuid) -> Result<Vec<Balance>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "select currency, balance from musubi.group_wallets where group_id = $1 order by currency",
        )
        .bind(group)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(currency, balance)| Balance { currency, balance })
            .collect())
    }

    pub async fn treasury_ledger(&self, group: Uuid, limit: i64) -> Result<Vec<TreasuryEntry>> {
        let rows: Vec<TreasuryRow> = sqlx::query_as(
            "select id, account_id, currency, delta, balance_after, reason, created_at
             from musubi.group_wallet_ledger where group_id = $1 order by created_at desc, id desc limit $2",
        )
        .bind(group)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| TreasuryEntry {
                id: r.0,
                account_id: r.1.map(AccountId),
                currency: r.2,
                delta: r.3,
                balance_after: r.4,
                reason: r.5,
                created_at: r.6,
            })
            .collect())
    }

    /// `account` pays `amount` of `currency` from their wallet into the group's treasury.
    pub async fn treasury_deposit(
        &self,
        group: Uuid,
        account: AccountId,
        currency: &str,
        amount: i64,
        reason: &str,
    ) -> Result<TreasuryEntry> {
        self.treasury_move(group, account, currency, amount, reason, true)
            .await
    }

    /// The treasury pays `amount` of `currency` out to `account`'s wallet (caller checked the role).
    pub async fn treasury_withdraw(
        &self,
        group: Uuid,
        account: AccountId,
        currency: &str,
        amount: i64,
        reason: &str,
    ) -> Result<TreasuryEntry> {
        self.treasury_move(group, account, currency, amount, reason, false)
            .await
    }

    /// Server-side credit to a group treasury with no member leg (tournament prizes, events).
    pub async fn treasury_grant(
        &self,
        group: Uuid,
        currency: &str,
        amount: i64,
        reason: &str,
    ) -> Result<TreasuryEntry> {
        if amount <= 0 {
            return Err(Error::InvalidArgument("amount must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let balance_after: i64 = sqlx::query_scalar(
            "insert into musubi.group_wallets (group_id, currency, balance) values ($1, $2, $3)
             on conflict (group_id, currency) do update
             set balance = musubi.group_wallets.balance + $3, updated_at = now()
             returning balance",
        )
        .bind(group)
        .bind(currency)
        .bind(amount)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let entry = TreasuryEntry {
            id: Uuid::now_v7(),
            account_id: None,
            currency: currency.to_string(),
            delta: amount,
            balance_after,
            reason: reason.to_string(),
            created_at: OffsetDateTime::now_utc(),
        };
        sqlx::query(
            "insert into musubi.group_wallet_ledger (id, group_id, account_id, currency, delta, balance_after, reason)
             values ($1, $2, null, $3, $4, $5, $6)",
        )
        .bind(entry.id)
        .bind(group)
        .bind(currency)
        .bind(amount)
        .bind(balance_after)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(entry)
    }

    async fn treasury_move(
        &self,
        group: Uuid,
        account: AccountId,
        currency: &str,
        amount: i64,
        reason: &str,
        deposit: bool,
    ) -> Result<TreasuryEntry> {
        if amount <= 0 {
            return Err(Error::InvalidArgument("amount must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let (delta, kind) = if deposit {
            (amount, "treasury_deposit")
        } else {
            (-amount, "treasury_withdraw")
        };
        // member leg first: a deposit fails here when the wallet is short
        Self::adjust_tx(
            &mut tx,
            account,
            currency,
            -delta,
            kind,
            &group.to_string(),
            None,
            serde_json::json!({ "group_id": group }),
        )
        .await?;
        // update first: an upsert would trip the balance >= 0 check on the proposed row of a payout
        let mut balance: Option<i64> = sqlx::query_scalar(
            "update musubi.group_wallets set balance = balance + $3, updated_at = now()
             where group_id = $1 and currency = $2 and balance + $3 >= 0 returning balance",
        )
        .bind(group)
        .bind(currency)
        .bind(delta)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if balance.is_none() && delta > 0 {
            balance = Some(
                sqlx::query_scalar(
                    "insert into musubi.group_wallets (group_id, currency, balance) values ($1, $2, $3) returning balance",
                )
                .bind(group)
                .bind(currency)
                .bind(delta)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?,
            );
        }
        let Some(balance_after) = balance else {
            return Err(Error::InvalidArgument("treasury balance too low".into()));
        };
        let entry = TreasuryEntry {
            id: Uuid::now_v7(),
            account_id: Some(account),
            currency: currency.to_string(),
            delta,
            balance_after,
            reason: reason.to_string(),
            created_at: OffsetDateTime::now_utc(),
        };
        sqlx::query(
            "insert into musubi.group_wallet_ledger (id, group_id, account_id, currency, delta, balance_after, reason)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(entry.id)
        .bind(group)
        .bind(account.0)
        .bind(currency)
        .bind(delta)
        .bind(balance_after)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(entry)
    }
}
