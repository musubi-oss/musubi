//! Persist + live-push notifications in one call.

use musubi_core::Result;
use musubi_proto::ServerMessage;
use musubi_social::{NewNotification, NotificationService};

use crate::fanout::Fanout;

#[derive(Clone)]
pub struct Notifier {
    pub service: NotificationService,
    fanout: Fanout,
}

impl Notifier {
    pub fn new(service: NotificationService, fanout: Fanout) -> Self {
        Self { service, fanout }
    }

    pub async fn send(&self, batch: Vec<NewNotification>) -> Result<()> {
        for (user, n) in self.service.send(batch).await? {
            self.fanout.account(
                user,
                ServerMessage::Notification {
                    id: n.id.to_string(),
                    subject: n.subject,
                    content: n.content,
                    code: n.code,
                    sender_id: n.sender_id,
                    create_time: n.create_time.unix_timestamp(),
                    persistent: n.persistent,
                },
            );
        }
        Ok(())
    }
}
