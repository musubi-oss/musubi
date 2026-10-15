//! Persist + live-push notifications in one call.

use musubi_core::Result;
use musubi_proto::ServerMessage;
use musubi_social::{NewNotification, NotificationService};

use crate::fanout::Fanout;

#[derive(Clone)]
pub struct Notifier {
    pub service: NotificationService,
    fanout: Fanout,
    push: Option<std::sync::Arc<crate::push::Push>>,
}

impl Notifier {
    pub fn new(service: NotificationService, fanout: Fanout) -> Self {
        Self {
            service,
            fanout,
            push: None,
        }
    }

    /// Push to registered devices when a notification reaches no live session on this node.
    pub fn with_push(mut self, push: std::sync::Arc<crate::push::Push>) -> Self {
        self.push = Some(push);
        self
    }

    pub fn push(&self) -> Option<&std::sync::Arc<crate::push::Push>> {
        self.push.as_ref()
    }

    pub async fn send(&self, batch: Vec<NewNotification>) -> Result<()> {
        for (user, n) in self.service.send(batch).await? {
            let live = self.fanout.account_live(
                user,
                ServerMessage::Notification {
                    id: n.id.to_string(),
                    subject: n.subject.clone(),
                    content: n.content.clone(),
                    code: n.code,
                    sender_id: n.sender_id,
                    create_time: n.create_time.unix_timestamp(),
                    persistent: n.persistent,
                },
            );
            if live == 0 && !self.fanout.is_online_anywhere(user) {
                if let Some(push) = self.push.clone().filter(|p| p.enabled()) {
                    tokio::spawn(async move {
                        if let Err(e) = push.send(user, &n.subject, &n.content, n.code).await {
                            tracing::warn!(error = %e, "push delivery failed");
                        }
                    });
                }
            }
        }
        Ok(())
    }
}
