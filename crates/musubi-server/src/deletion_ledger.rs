//! Backup re-deletion safeguard (Metaplay parity): `musubi.account_deletions` records every
//! account deletion; when a deleted account is alive again (database restored from a backup,
//! rows re-imported) the sweeper deletes it again — unless an operator lifted the entry or
//! restored the account with `override_deletion`. The ledger can be exported and re-imported so
//! it outlives the backup it is protecting against.

use musubi_core::{AccountId, Error, Result};
use serde_json::json;
use uuid::Uuid;

use crate::app::AppState;

/// Re-delete every ledgered account that is alive and not lifted; returns the account ids.
pub async fn sweep(s: &AppState) -> Result<Vec<Uuid>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select d.account_id from musubi.account_deletions d
         join musubi.accounts a on a.id = d.account_id
         where d.lifted_at is null and a.deleted_at is null
         order by d.deleted_at limit 1000",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut done = Vec::new();
    for id in ids {
        let original: Option<time::OffsetDateTime> = sqlx::query_scalar(
            "select deleted_at from musubi.account_deletions where account_id = $1",
        )
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
        if let Err(e) = s.accounts.delete(AccountId(id)).await {
            tracing::warn!(error = %e, %id, "re-deletion failed");
            continue;
        }
        sqlx::query(
            "update musubi.account_deletions set deleted_at = coalesce($2, deleted_at),
                 reapplied = reapplied + 1, last_reapplied_at = now() where account_id = $1",
        )
        .bind(id)
        .bind(original)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        s.hub.kick_account(AccountId(id));
        s.events
            .emit(
                "account.redeleted",
                Some(AccountId(id)),
                json!({ "originally_deleted_at": original.map(|t| t.unix_timestamp()) }),
            )
            .await;
        done.push(id);
    }
    Ok(done)
}
