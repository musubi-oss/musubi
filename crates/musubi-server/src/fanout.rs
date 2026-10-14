//! Deliver realtime messages to sessions on this node and, through the cluster bus, on every other node.

use std::sync::Arc;

use musubi_cluster::{BusMessage, Cluster};
use musubi_core::AccountId;
use musubi_proto::ServerMessage;
use serde::{Deserialize, Serialize};

use crate::realtime::Hub;

#[derive(Serialize, Deserialize)]
enum Wire {
    Account {
        account_id: AccountId,
        msg: ServerMessage,
    },
    Channel {
        channel_id: String,
        msg: ServerMessage,
    },
}

const TOPIC: &str = "realtime";

#[derive(Clone)]
pub struct Fanout {
    hub: Arc<Hub>,
    cluster: Option<Arc<Cluster>>,
}

impl Fanout {
    pub fn new(hub: Arc<Hub>, cluster: Option<Arc<Cluster>>) -> Self {
        let f = Self { hub, cluster };
        if let Some(c) = &f.cluster {
            let mut rx = c.subscribe();
            let hub = f.hub.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(m) => apply(&hub, &m),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(dropped = n, "fanout bus lagged");
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        f
    }

    pub fn account(&self, account_id: AccountId, msg: ServerMessage) {
        self.hub.send_to_account(account_id, msg.clone());
        self.publish(Wire::Account { account_id, msg });
    }

    pub fn channel(&self, channel_id: &str, msg: ServerMessage) {
        self.hub.broadcast_channel(channel_id, msg.clone());
        self.publish(Wire::Channel {
            channel_id: channel_id.to_string(),
            msg,
        });
    }

    fn publish(&self, w: Wire) {
        let Some(c) = self.cluster.clone() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = c.publish(TOPIC, &w).await {
                tracing::warn!(error = %e, "fanout publish failed");
            }
        });
    }
}

fn apply(hub: &Hub, m: &BusMessage) {
    if m.topic != TOPIC {
        return;
    }
    match m.decode::<Wire>() {
        Ok(Wire::Account { account_id, msg }) => {
            hub.send_to_account(account_id, msg);
        }
        Ok(Wire::Channel { channel_id, msg }) => {
            hub.broadcast_channel(&channel_id, msg);
        }
        Err(e) => tracing::warn!(error = %e, "bad fanout message"),
    }
}
