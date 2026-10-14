//! Persist + live-push notifications in one call.

use std::sync::Arc;

use musubi_core::Result;
use musubi_proto::ServerMessage;
use musubi_social::{NewNotification, NotificationService};

use crate::realtime::Hub;

#[derive(Clone)]
pub struct Notifier {
    pub service: NotificationService,
    hub: Arc<Hub>,
}

impl Notifier {
    pub fn new(service: NotificationService, hub: Arc<Hub>) -> Self {
        Self { service, hub }
    }

    pub async fn send(&self, batch: Vec<NewNotification>) -> Result<()> {
        for (user, n) in self.service.send(batch).await? {
            self.hub.send_to_account(
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
