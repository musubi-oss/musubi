//! Imported platform friend ids (Steam/PSN/Xbox/Discord/...) are remembered per account, so a
//! friend who installs the game later is matched the moment their identity is linked: one node
//! (lease `platform_friends`) joins unmatched rows against `musubi.identities`, emits
//! `friend.platform_match` and, when the import asked for it, sends the friend request.

use musubi_core::{AccountId, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::app::AppState;

/// Replace the stored list for `provider` with `subjects`; rows already matched keep their match.
pub async fn remember(
    s: &AppState,
    me: AccountId,
    provider: &str,
    subjects: &[String],
    auto_add: bool,
) -> Result<()> {
    let mut tx = s.db.begin().await.map_err(musubi_core::Error::internal)?;
    sqlx::query(
        "delete from musubi.platform_friends where account_id = $1 and provider = $2 and not (subject = any($3))",
    )
    .bind(me.0)
    .bind(provider)
    .bind(subjects)
    .execute(&mut *tx)
    .await
    .map_err(musubi_core::Error::internal)?;
    sqlx::query(
        "insert into musubi.platform_friends (account_id, provider, subject, auto_add)
         select $1, $2, s, $4 from unnest($3::text[]) s
         on conflict (account_id, provider, subject) do update set auto_add = excluded.auto_add",
    )
    .bind(me.0)
    .bind(provider)
    .bind(subjects)
    .bind(auto_add)
    .execute(&mut *tx)
    .await
    .map_err(musubi_core::Error::internal)?;
    tx.commit().await.map_err(musubi_core::Error::internal)
}

pub async fn mark_matched(
    s: &AppState,
    me: AccountId,
    provider: &str,
    subject: &str,
    account: AccountId,
) -> Result<()> {
    sqlx::query(
        "update musubi.platform_friends set matched_account = $4, matched_at = now()
         where account_id = $1 and provider = $2 and subject = $3 and matched_account is null",
    )
    .bind(me.0)
    .bind(provider)
    .bind(subject)
    .bind(account.0)
    .execute(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    Ok(())
}

/// Stored ids for the caller (optionally one provider) with the account each resolved to.
pub async fn list(s: &AppState, me: AccountId, provider: Option<&str>) -> Result<Value> {
    let rows = sqlx::query(
        "select pf.provider, pf.subject, pf.auto_add, pf.matched_account, a.username
         from musubi.platform_friends pf
         left join musubi.accounts a on a.id = pf.matched_account
         where pf.account_id = $1 and ($2::text is null or pf.provider = $2)
         order by pf.provider, pf.subject",
    )
    .bind(me.0)
    .bind(provider)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    Ok(json!({
        "friends": rows.iter().map(|r| json!({
            "provider": r.get::<String, _>(0),
            "subject": r.get::<String, _>(1),
            "auto_add": r.get::<bool, _>(2),
            "account_id": r.get::<Option<Uuid>, _>(3),
            "username": r.get::<Option<String>, _>(4),
        })).collect::<Vec<_>>()
    }))
}

/// Match stored ids against identities linked since the import; returns how many matched.
pub async fn run_match(s: &AppState, limit: i64) -> Result<usize> {
    let rows = sqlx::query(
        "select pf.account_id, pf.provider, pf.subject, pf.auto_add, i.account_id, a.username
         from musubi.platform_friends pf
         join musubi.identities i on i.provider = pf.provider and i.subject = pf.subject
         join musubi.accounts a on a.id = i.account_id and a.disabled_at is null
         where pf.matched_account is null and i.account_id <> pf.account_id
         limit $1",
    )
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let mut n = 0;
    for r in rows {
        let importer = AccountId(r.get::<Uuid, _>(0));
        let provider: String = r.get(1);
        let subject: String = r.get(2);
        let auto_add: bool = r.get(3);
        let friend = AccountId(r.get::<Uuid, _>(4));
        let username: String = r.get(5);
        mark_matched(s, importer, &provider, &subject, friend).await?;
        n += 1;
        s.events
            .emit(
                "friend.platform_match",
                Some(importer),
                json!({ "provider": provider, "subject": subject, "account_id": friend, "username": username, "auto_add": auto_add }),
            )
            .await;
        if auto_add {
            match s.social.add(importer, &[friend]).await {
                Ok(outcomes) => {
                    let _ = s
                        .events
                        .emit(
                            "friend.added",
                            Some(importer),
                            json!({ "ids": [friend], "outcomes": outcomes, "import": provider }),
                        )
                        .await;
                }
                Err(e) => {
                    tracing::warn!(%importer, %friend, error = %e, "platform friend auto-add failed")
                }
            }
        }
    }
    Ok(n)
}

/// Leader loop: one node matches new identities every `social.platform_match_secs`.
pub fn start_matcher(state: &AppState) {
    let secs = state.cfg.social.platform_match_secs;
    if secs == 0 {
        return;
    }
    let st = state.clone();
    let lease = st
        .cluster
        .clone()
        .map(|c| c.lease("platform_friends", musubi_cluster::DEAD_AFTER));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
        loop {
            tick.tick().await;
            if let Some(l) = &lease {
                if !l.held() {
                    continue;
                }
            }
            match run_match(&st, 1000).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(matched = n, "platform friends matched"),
                Err(e) => tracing::warn!(error = %e, "platform friend matcher failed"),
            }
        }
    });
}
