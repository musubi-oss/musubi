//! What a simulator sends down its stream (`/v1/server/worlds/{w}/stream?sim=`): one typed
//! enum tagged by `op`, the same JSON the stream always took (`sim_command_wire` pins it). The
//! `id` a simulator attaches for matching replies and the `sim` the HTTP forms carry are not
//! part of the command and are read beside it (`SimCommandFrame`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::CellId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SimCommand {
    /// Take (or renew) these cells; the reply's `cells` are the ones granted.
    Lease {
        cells: Vec<CellId>,
    },
    Spawn {
        x: f64,
        y: f64,
        #[serde(default)]
        components: Value,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        persistent: bool,
        /// Delivered to every subscriber whatever their interest.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        global: bool,
        /// Wait in the cell's admission queue instead of failing at `soft_cap`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queue: bool,
        /// Despawn after this many ticks without a write, interaction or handover.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decay_ticks: Option<u64>,
    },
    /// The simulator finished its tick `tick`; renews its leases.
    TickDone {
        tick: u64,
    },
    Despawn {
        entity: String,
    },
    Attach {
        entity: String,
        root: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        contained: bool,
    },
    Detach {
        entity: String,
    },
    /// Write components and/or a position (`x` and `y` go together).
    Apply {
        entity: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
        #[serde(default)]
        components: Value,
    },
    /// Act on an entity another simulator may own (`tick`/`x`/`y`/`tolerance`: where the actor
    /// saw it when it acted, for the owner's rewind check).
    Interact {
        entity: String,
        kind: String,
        #[serde(default)]
        payload: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tick: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tolerance: Option<f64>,
    },
    /// Pin `with` into `entity`'s cell for `ttl_ticks`.
    Colocate {
        entity: String,
        with: String,
        #[serde(default = "default_colocate_ttl")]
        ttl_ticks: u64,
    },
    /// Where was `entity` at `tick` (and did `x`/`y` within `tolerance` hit it)?
    Rewind {
        entity: String,
        tick: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tolerance: Option<f64>,
    },
    /// Accept the entity offered by `handover_request`.
    HandoverAck {
        entity: String,
    },
    /// Engine-level networking: one opaque packet for the client session behind `conn` (see the
    /// `net_packet` event). Fire-and-forget: the reply only says whether the session was known.
    NetPacket {
        conn: u64,
        session: Value,
        #[serde(default)]
        data: Vec<u8>,
    },
    /// The simulator closed engine connection `conn` on `entity`; the client gets `net_close`.
    NetClose {
        conn: u64,
        session: Value,
        entity: String,
        #[serde(default)]
        reason: String,
    },
    /// Engine-level networking, control lane: opaque bytes for the client session behind `conn`
    /// delivered as `net_meta` — reliable and ordered with respect to other control traffic
    /// (unlike `net_packet`), for what the engine must know before its own packets arrive
    /// (e.g. the NetGUID table a new owner hands a rehomed client).
    NetMeta {
        conn: u64,
        session: Value,
        #[serde(default)]
        data: Vec<u8>,
    },
}

pub fn default_colocate_ttl() -> u64 {
    300
}

impl SimCommand {
    /// Every `op` the stream understands.
    pub const OPS: &'static [&'static str] = &[
        "lease",
        "spawn",
        "tick_done",
        "despawn",
        "attach",
        "detach",
        "apply",
        "interact",
        "colocate",
        "rewind",
        "handover_ack",
        "net_packet",
        "net_close",
    ];

    pub fn op(&self) -> &'static str {
        match self {
            Self::Lease { .. } => "lease",
            Self::Spawn { .. } => "spawn",
            Self::TickDone { .. } => "tick_done",
            Self::Despawn { .. } => "despawn",
            Self::Attach { .. } => "attach",
            Self::Detach { .. } => "detach",
            Self::Apply { .. } => "apply",
            Self::Interact { .. } => "interact",
            Self::Colocate { .. } => "colocate",
            Self::Rewind { .. } => "rewind",
            Self::HandoverAck { .. } => "handover_ack",
            Self::NetPacket { .. } => "net_packet",
            Self::NetClose { .. } => "net_close",
            Self::NetMeta { .. } => "net_meta",
        }
    }

    /// The command as one text frame for the stream.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("sim command serializes")
    }

    /// With a caller-chosen `id` echoed in the reply.
    pub fn with_id(self, id: impl Into<Value>) -> SimCommandFrame {
        SimCommandFrame {
            id: Some(id.into()),
            cmd: self,
        }
    }
}

/// A command as it travels: the `op`-tagged command plus the optional `id` echoed in the reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimCommandFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(flatten)]
    pub cmd: SimCommand,
}

impl SimCommandFrame {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("sim command serializes")
    }
}

#[cfg(test)]
mod sim_command_wire {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_the_stream_json() {
        let c = CellId {
            x: 1,
            y: 0,
            z: 0,
            level: 0,
        };
        let cases: Vec<(SimCommand, Value)> = vec![
            (
                SimCommand::Lease { cells: vec![c] },
                json!({"op": "lease", "cells": [{"x": 1, "y": 0}]}),
            ),
            (
                SimCommand::Spawn {
                    x: 1.5,
                    y: 2.0,
                    components: json!({"hp": 7}),
                    persistent: false,
                    global: false,
                    queue: false,
                    decay_ticks: None,
                },
                json!({"op": "spawn", "x": 1.5, "y": 2.0, "components": {"hp": 7}}),
            ),
            (
                SimCommand::TickDone { tick: 9 },
                json!({"op": "tick_done", "tick": 9}),
            ),
            (
                SimCommand::Despawn {
                    entity: "e1".into(),
                },
                json!({"op": "despawn", "entity": "e1"}),
            ),
            (
                SimCommand::Attach {
                    entity: "e1".into(),
                    root: "r".into(),
                    contained: true,
                },
                json!({"op": "attach", "entity": "e1", "root": "r", "contained": true}),
            ),
            (
                SimCommand::Detach {
                    entity: "e1".into(),
                },
                json!({"op": "detach", "entity": "e1"}),
            ),
            (
                SimCommand::Apply {
                    entity: "e1".into(),
                    x: Some(3.0),
                    y: Some(4.0),
                    components: json!({"hp": 1}),
                },
                json!({"op": "apply", "entity": "e1", "x": 3.0, "y": 4.0, "components": {"hp": 1}}),
            ),
            (
                SimCommand::Interact {
                    entity: "e1".into(),
                    kind: "hit".into(),
                    payload: json!(3),
                    tick: Some(5),
                    x: None,
                    y: None,
                    tolerance: None,
                },
                json!({"op": "interact", "entity": "e1", "kind": "hit", "payload": 3, "tick": 5}),
            ),
            (
                SimCommand::Colocate {
                    entity: "a".into(),
                    with: "b".into(),
                    ttl_ticks: 300,
                },
                json!({"op": "colocate", "entity": "a", "with": "b", "ttl_ticks": 300}),
            ),
            (
                SimCommand::Rewind {
                    entity: "e1".into(),
                    tick: 7,
                    x: Some(1.0),
                    y: Some(2.0),
                    tolerance: Some(0.5),
                },
                json!({"op": "rewind", "entity": "e1", "tick": 7, "x": 1.0, "y": 2.0, "tolerance": 0.5}),
            ),
            (
                SimCommand::HandoverAck {
                    entity: "e1".into(),
                },
                json!({"op": "handover_ack", "entity": "e1"}),
            ),
        ];
        for (cmd, wire) in cases {
            assert_eq!(serde_json::to_value(&cmd).unwrap(), wire, "{cmd:?}");
            let back: SimCommand = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(back, cmd, "{wire}");
            assert_eq!(wire["op"], cmd.op());
        }
        // the frame: `id` and the HTTP form's `sim` beside the command, defaults filled in
        let f: SimCommandFrame = serde_json::from_value(
            json!({"op": "colocate", "id": 4, "sim": "x", "entity": "a", "with": "b"}),
        )
        .unwrap();
        assert_eq!(f.id, Some(json!(4)));
        assert_eq!(
            f.cmd,
            SimCommand::Colocate {
                entity: "a".into(),
                with: "b".into(),
                ttl_ticks: 300
            }
        );
        assert!(serde_json::from_value::<SimCommandFrame>(json!({"op": "fly"})).is_err());
        assert_eq!(
            SimCommand::HandoverAck { entity: "e".into() }
                .with_id(1)
                .to_json(),
            r#"{"id":1,"op":"handover_ack","entity":"e"}"#
        );
    }
}
