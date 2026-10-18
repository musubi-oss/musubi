//! External chat automod: every message is POSTed to a webhook that may allow, block or
//! replace it. Built-in filters and the `before_chat_message` WASM hook still run first.

use std::sync::OnceLock;
use std::time::Duration;

use musubi_core::{config::ChatConfig, Error, Result};
use musubi_proto::UserPresence;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Automod {
    url: String,
    token: String,
    timeout: Duration,
    fail_open: bool,
}

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// What the webhook decided.
pub enum Verdict {
    Allow,
    Replace(Value),
    Block(String),
}

impl Automod {
    pub fn from_cfg(c: &ChatConfig) -> Option<Self> {
        if c.automod_url.is_empty() {
            return None;
        }
        Some(Self {
            url: c.automod_url.clone(),
            token: c.automod_token.clone(),
            timeout: Duration::from_millis(c.automod_timeout_ms.max(50)),
            fail_open: c.automod_fail_open,
        })
    }

    pub async fn check(
        &self,
        channel_id: &str,
        sender: &UserPresence,
        content: &Value,
    ) -> Result<Verdict> {
        let client = CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .user_agent("musubi-automod")
                .build()
                .expect("reqwest client")
        });
        let mut req = client.post(&self.url).timeout(self.timeout).json(&json!({
            "channel_id": channel_id,
            "sender_id": sender.account_id,
            "username": sender.username,
            "content": content,
        }));
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        let res = async {
            let r = req.send().await?;
            let r = r.error_for_status()?;
            r.json::<Value>().await
        }
        .await;
        let v = match res {
            Ok(v) => v,
            Err(e) => {
                metrics::counter!("musubi_chat_automod_errors_total").increment(1);
                tracing::warn!(error = %e, "chat automod webhook failed");
                return if self.fail_open {
                    Ok(Verdict::Allow)
                } else {
                    Err(Error::Unavailable("chat moderation unavailable".into()))
                };
            }
        };
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("allow");
        metrics::counter!("musubi_chat_automod_total", "action" => action.to_string()).increment(1);
        Ok(match action {
            "block" | "reject" => Verdict::Block(
                v.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("message rejected by moderation")
                    .to_string(),
            ),
            "replace" => match v.get("content") {
                Some(c) if c.is_object() => Verdict::Replace(c.clone()),
                _ => Verdict::Allow,
            },
            _ => Verdict::Allow,
        })
    }
}
