//! Chat translation: when `chat.translate_url` is set, a message whose `content.text` is a
//! string is POSTed to the webhook together with the sender's language and the languages of
//! everyone present in the channel; the answer `{"translations": {"de": "...", "ru": "..."}}`
//! is attached to the message as `content.translations` (stored with it, so history has it too)
//! and each client shows its own language. The webhook is only called when someone present
//! speaks another language; errors and timeouts never hold the message back.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use musubi_core::{config::ChatConfig, AccountId};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct Translator {
    url: String,
    token: String,
    timeout: Duration,
    max_langs: usize,
    db: PgPool,
}

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
/// account → (lang, fetched at); languages change rarely, one minute of staleness is fine.
static LANGS: OnceLock<DashMap<Uuid, (String, Instant)>> = OnceLock::new();
const LANG_TTL: Duration = Duration::from_secs(60);
const MAX_TEXT: usize = 4096;

/// `en-US` → `en`; empty/odd values → `en`.
fn base_lang(s: &str) -> String {
    let b = s
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if b.is_empty() || b.len() > 8 {
        "en".into()
    } else {
        b
    }
}

impl Translator {
    pub fn from_cfg(c: &ChatConfig, db: PgPool) -> Option<Self> {
        if c.translate_url.is_empty() {
            return None;
        }
        Some(Self {
            url: c.translate_url.clone(),
            token: c.translate_token.clone(),
            timeout: Duration::from_millis(c.translate_timeout_ms.max(50)),
            max_langs: c.translate_max_langs.clamp(1, 32),
            db,
        })
    }

    async fn langs(&self, ids: &[AccountId]) -> HashMap<Uuid, String> {
        let cache = LANGS.get_or_init(DashMap::new);
        let now = Instant::now();
        let mut out = HashMap::new();
        let mut missing = Vec::new();
        for id in ids {
            match cache.get(&id.0) {
                Some(e) if now.duration_since(e.1) < LANG_TTL => {
                    out.insert(id.0, e.0.clone());
                }
                _ => missing.push(id.0),
            }
        }
        if !missing.is_empty() {
            let rows: Vec<(Uuid, String)> =
                sqlx::query_as("select id, lang from musubi.accounts where id = any($1)")
                    .bind(&missing)
                    .fetch_all(&self.db)
                    .await
                    .unwrap_or_default();
            for (id, lang) in rows {
                let lang = base_lang(&lang);
                cache.insert(id, (lang.clone(), now));
                out.insert(id, lang);
            }
        }
        out
    }

    /// Drop a cached language (account language changed).
    pub fn forget(account: AccountId) {
        if let Some(c) = LANGS.get() {
            c.remove(&account.0);
        }
    }

    /// Attach `translations` to `content` for the languages of `members` other than the
    /// sender's. Never fails: on any error the message goes out untranslated.
    pub async fn translate(
        &self,
        channel_id: &str,
        sender: AccountId,
        members: &[AccountId],
        content: &mut Value,
    ) {
        let Some(text) = content.get("text").and_then(Value::as_str) else {
            return;
        };
        if text.trim().is_empty() || text.len() > MAX_TEXT {
            return;
        }
        let mut ids: Vec<AccountId> = members.to_vec();
        if !ids.contains(&sender) {
            ids.push(sender);
        }
        let langs = self.langs(&ids).await;
        let source = langs.get(&sender.0).cloned().unwrap_or_else(|| "en".into());
        let mut targets: Vec<String> = langs
            .iter()
            .filter(|(id, l)| **id != sender.0 && **l != source)
            .map(|(_, l)| l.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if targets.is_empty() {
            return;
        }
        targets.sort();
        targets.truncate(self.max_langs);
        let client = CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .user_agent("musubi-translate")
                .build()
                .expect("reqwest client")
        });
        let mut req = client.post(&self.url).timeout(self.timeout).json(&json!({
            "channel_id": channel_id,
            "sender_id": sender,
            "text": text,
            "source": source,
            "targets": targets,
        }));
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        let text_owned = text.to_string();
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "chat translate webhook failed");
                return;
            }
        };
        if !resp.status().is_success() {
            tracing::warn!(status = %resp.status(), "chat translate webhook rejected");
            return;
        }
        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "chat translate webhook: bad JSON");
                return;
            }
        };
        let Some(map) = body.get("translations").and_then(Value::as_object) else {
            return;
        };
        let mut out = serde_json::Map::new();
        for lang in &targets {
            if let Some(t) = map.get(lang).and_then(Value::as_str) {
                if !t.is_empty() && t.len() <= MAX_TEXT && t != text_owned {
                    out.insert(lang.clone(), Value::String(t.to_string()));
                }
            }
        }
        if out.is_empty() {
            return;
        }
        if let Some(obj) = content.as_object_mut() {
            obj.insert("translations".into(), Value::Object(out));
            obj.insert("source_lang".into(), Value::String(source));
        }
    }
}
