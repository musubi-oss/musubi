//! Mirrors unlocked achievements to platforms. Steam is unlocked server-to-server
//! (ISteamUserStats/SetUserStatsForGame with the publisher key from `[iap.steam]`); PSN trophies and
//! Xbox achievements can only be unlocked by the client SDK, so the `achievement.unlocked` event
//! carries every `platform_ids` entry for it.

use musubi_core::AccountId;
use musubi_economy::AchievementUpdate;
use serde_json::json;

use crate::app::AppState;

pub async fn after_update(s: &AppState, account: AccountId, u: &AchievementUpdate) {
    if !u.unlocked {
        return;
    }
    s.events
        .emit(
            "achievement.unlocked",
            Some(account),
            json!({ "achievement_id": u.achievement_id, "platform_ids": u.platform_ids }),
        )
        .await;
    let steam = &s.cfg.iap.steam;
    if let Some(name) = u
        .platform_ids
        .get("steam")
        .filter(|_| !steam.key.is_empty())
    {
        let (s2, name, id) = (s.clone(), name.clone(), u.achievement_id.clone());
        tokio::spawn(async move {
            if let Err(e) = steam_unlock(&s2, account, &name).await {
                tracing::warn!(%account, achievement = %id, error = %e, "steam achievement sync failed");
                s2.events
                    .emit(
                        "achievement.sync_failed",
                        Some(account),
                        json!({ "achievement_id": id, "platform": "steam", "error": e }),
                    )
                    .await;
            }
        });
    }
}

async fn steam_unlock(s: &AppState, account: AccountId, api_name: &str) -> Result<(), String> {
    let steam = &s.cfg.iap.steam;
    let steam_id: Option<String> = sqlx::query_scalar(
        "select subject from musubi.identities where account_id = $1 and provider = 'steam' limit 1",
    )
    .bind(account.0)
    .fetch_optional(&s.db)
    .await
    .map_err(|e| e.to_string())?;
    let Some(steam_id) = steam_id else {
        return Err("no linked steam identity".into());
    };
    let app_id = steam.app_id.to_string();
    let form = [
        ("key", steam.key.as_str()),
        ("appid", app_id.as_str()),
        ("steamid", steam_id.as_str()),
        ("count", "1"),
        ("name[0]", api_name),
        ("value[0]", "1"),
    ];
    let url = format!(
        "{}/ISteamUserStats/SetUserStatsForGame/v1/",
        steam.url.trim_end_matches('/')
    );
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent("musubi-achievements")
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .expect("reqwest client")
    });
    let resp = client
        .post(url)
        .form(&form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() || body["response"]["result"] != 1 {
        return Err(format!("steam {status}: {body}"));
    }
    Ok(())
}
