//! What a world tells its simulators (`GET /v1/server/worlds/{w}/events`, the `events` of a
//! stream `tick` frame): one typed enum, tagged by `type`, serialized exactly like the ad-hoc
//! JSON it replaces (`sim_event_wire` tests pin every variant). Generic over the entity payload
//! so the server fills it with its wire entity type and a simulator reads it back as the same
//! type (or as raw JSON with `SimEvent<serde_json::Value>`, the default). Note for payload
//! types: a `type`-tagged enum is decoded through serde's buffered content, which cannot hand
//! out raw JSON text — `musubi_proto::JsonText` handles that itself (re-serializing), a custom
//! raw-text type must do the same.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{CellId, EntityId, Pos, SimId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SimEvent<E = Value> {
    /// The controller gave the simulator a cell (`from`: who had it before).
    CellAssigned {
        cell: CellId,
        from: Option<SimId>,
        reason: String,
    },
    /// The cell was taken away (`to`: who gets it).
    CellRevoked {
        cell: CellId,
        to: SimId,
        reason: String,
    },
    /// The cell moved to another node; `entities` were shipped there.
    CellReleased {
        cell: CellId,
        node: Uuid,
        entities: usize,
    },
    /// The cell came back from hibernation with `entities` restored.
    CellReloaded {
        cell: CellId,
        entities: usize,
    },
    /// Quiet cell put to sleep.
    CellHibernated {
        cell: CellId,
        ephemeral_despawned: usize,
        persistent_kept: usize,
    },
    /// A spawn hit the soft cap of the cell.
    CellFull {
        cell: CellId,
        soft_cap: u32,
    },
    /// Spawn waiting for room; `position` in the cell's queue, `eta_ticks` estimated.
    Queued {
        ticket: Uuid,
        cell: CellId,
        position: usize,
        /// `None` when the queue cannot estimate (no departures seen yet).
        eta_ticks: Option<u64>,
        soft_cap: u32,
    },
    QueueUpdate {
        ticket: Uuid,
        cell: CellId,
        position: usize,
        /// `None` when the queue cannot estimate (no departures seen yet).
        eta_ticks: Option<u64>,
    },
    /// The queued spawn happened; `entity` is live.
    QueueAdmitted {
        ticket: Uuid,
        cell: CellId,
        entity: E,
    },
    QueueExpired {
        ticket: Uuid,
        cell: CellId,
        reason: String,
    },
    /// An entity with `decay_ticks` ran out and was despawned.
    Decayed {
        entity: EntityId,
        cell: CellId,
    },
    /// Time dilation of a cell changed.
    TimeScale {
        cell: CellId,
        scale: f64,
    },
    /// The receiving simulator is asked to take `entity` (full `state`, attached `group`); it
    /// answers with `handover_ack`.
    HandoverRequest {
        entity: EntityId,
        from: SimId,
        state: Option<E>,
        group: Vec<E>,
    },
    /// The owning simulator is told its entity is leaving (`node` set for a cross-node move,
    /// `group` for the attached entities of a local one).
    HandoverStarted {
        entity: EntityId,
        to: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<Vec<EntityId>>,
    },
    HandoverCompleted {
        entity: EntityId,
        to: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<Uuid>,
    },
    HandoverCancelled {
        entity: EntityId,
        reason: String,
    },
    /// An entity arrived from another node onto this simulator's cell, with the inputs that
    /// were buffered for it on the way.
    HandoverArrived {
        entity: EntityId,
        from_node: Uuid,
        state: E,
        buffered_inputs: Vec<Value>,
    },
    /// A client input routed to the entity's owner.
    Input {
        entity: EntityId,
        session: Value,
        input: Value,
    },
    /// Another simulator (or client) acted on this simulator's entity; `rewind` says where the
    /// target was at `at_tick`, `seq`/`tick` are stamped by the owner.
    Interaction {
        entity: EntityId,
        kind: String,
        payload: Value,
        from_sim: SimId,
        at_tick: Option<u64>,
        at: Option<Pos>,
        tolerance: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rewind: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tick: Option<u64>,
    },
    /// `entity` is pinned into `cell` (next to `with`) by simulator `by` until `until_tick`.
    CoLocate {
        entity: EntityId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        with: Option<EntityId>,
        cell: CellId,
        by: SimId,
        until_tick: u64,
    },
    TxCommitted {
        tx: Uuid,
        entity: EntityId,
    },
    TxAborted {
        tx: Uuid,
        entity: EntityId,
        reason: String,
    },
    /// Engine-level networking relayed through the world (Unreal `UMusubiNetDriver`): one opaque
    /// packet from client session `session` for `entity`, which this simulator owns. `conn`
    /// identifies the engine connection; the first packet on an unknown `conn` opens it (the
    /// simulator answers with `net_packet` commands on the same `conn`/`session`).
    NetPacket {
        conn: u64,
        session: Value,
        entity: EntityId,
        data: Vec<u8>,
    },
    /// The client (or the world, after a handover: `reason = "rehome"`) closed engine connection
    /// `conn` on `entity`.
    NetClose {
        conn: u64,
        session: Value,
        entity: EntityId,
        reason: String,
    },
    /// An event this build does not know (a newer server); carried, not an error.
    #[serde(other)]
    Unknown,
}

impl<E: Serialize> SimEvent<E> {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("sim event serializes")
    }
}

impl<E: Serialize> From<SimEvent<E>> for Value {
    fn from(e: SimEvent<E>) -> Value {
        e.to_value()
    }
}

#[cfg(test)]
mod sim_event_wire {
    use super::*;
    use serde_json::json;

    fn cell() -> CellId {
        CellId {
            x: 1,
            y: -2,
            z: 0,
            level: 0,
        }
    }

    /// Every variant serializes to the JSON the server used to build by hand (and reads back).
    #[test]
    fn matches_the_hand_built_json() {
        let (a, b, s) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let c = cell();
        let cases: Vec<(SimEvent<Value>, Value)> = vec![
            (
                SimEvent::CellAssigned {
                    cell: c,
                    from: None,
                    reason: "split".into(),
                },
                json!({"type": "cell_assigned", "cell": c, "from": null, "reason": "split"}),
            ),
            (
                SimEvent::CellRevoked {
                    cell: c,
                    to: s,
                    reason: "rebalance".into(),
                },
                json!({"type": "cell_revoked", "cell": c, "to": s, "reason": "rebalance"}),
            ),
            (
                SimEvent::CellReleased {
                    cell: c,
                    node: a,
                    entities: 2,
                },
                json!({"type": "cell_released", "cell": c, "node": a, "entities": 2}),
            ),
            (
                SimEvent::CellReloaded {
                    cell: c,
                    entities: 3,
                },
                json!({"type": "cell_reloaded", "cell": c, "entities": 3}),
            ),
            (
                SimEvent::CellHibernated {
                    cell: c,
                    ephemeral_despawned: 1,
                    persistent_kept: 4,
                },
                json!({"type": "cell_hibernated", "cell": c, "ephemeral_despawned": 1, "persistent_kept": 4}),
            ),
            (
                SimEvent::CellFull {
                    cell: c,
                    soft_cap: 500,
                },
                json!({"type": "cell_full", "cell": c, "soft_cap": 500}),
            ),
            (
                SimEvent::Queued {
                    ticket: a,
                    cell: c,
                    position: 3,
                    eta_ticks: Some(90),
                    soft_cap: 500,
                },
                json!({"type": "queued", "ticket": a, "cell": c, "position": 3, "eta_ticks": 90, "soft_cap": 500}),
            ),
            (
                SimEvent::QueueUpdate {
                    ticket: a,
                    cell: c,
                    position: 1,
                    eta_ticks: None,
                },
                json!({"type": "queue_update", "ticket": a, "cell": c, "position": 1, "eta_ticks": null}),
            ),
            (
                SimEvent::QueueAdmitted {
                    ticket: a,
                    cell: c,
                    entity: json!({"id": b}),
                },
                json!({"type": "queue_admitted", "ticket": a, "cell": c, "entity": {"id": b}}),
            ),
            (
                SimEvent::QueueExpired {
                    ticket: a,
                    cell: c,
                    reason: "timeout".into(),
                },
                json!({"type": "queue_expired", "ticket": a, "cell": c, "reason": "timeout"}),
            ),
            (
                SimEvent::Decayed { entity: b, cell: c },
                json!({"type": "decayed", "entity": b, "cell": c}),
            ),
            (
                SimEvent::TimeScale {
                    cell: c,
                    scale: 0.5,
                },
                json!({"type": "time_scale", "cell": c, "scale": 0.5}),
            ),
            (
                SimEvent::HandoverRequest {
                    entity: b,
                    from: s,
                    state: Some(json!({"id": b})),
                    group: vec![],
                },
                json!({"type": "handover_request", "entity": b, "from": s, "state": {"id": b}, "group": []}),
            ),
            (
                SimEvent::HandoverStarted {
                    entity: b,
                    to: a,
                    node: Some(a),
                    group: None,
                },
                json!({"type": "handover_started", "entity": b, "to": a, "node": a}),
            ),
            (
                SimEvent::HandoverStarted {
                    entity: b,
                    to: s,
                    node: None,
                    group: Some(vec![a]),
                },
                json!({"type": "handover_started", "entity": b, "to": s, "group": [a]}),
            ),
            (
                SimEvent::HandoverCompleted {
                    entity: b,
                    to: s,
                    node: None,
                },
                json!({"type": "handover_completed", "entity": b, "to": s}),
            ),
            (
                SimEvent::HandoverCompleted {
                    entity: b,
                    to: a,
                    node: Some(a),
                },
                json!({"type": "handover_completed", "entity": b, "to": a, "node": a}),
            ),
            (
                SimEvent::HandoverCancelled {
                    entity: b,
                    reason: "timeout".into(),
                },
                json!({"type": "handover_cancelled", "entity": b, "reason": "timeout"}),
            ),
            (
                SimEvent::HandoverArrived {
                    entity: b,
                    from_node: a,
                    state: json!({"id": b}),
                    buffered_inputs: vec![json!({"k": 1})],
                },
                json!({"type": "handover_arrived", "entity": b, "from_node": a, "state": {"id": b}, "buffered_inputs": [{"k": 1}]}),
            ),
            (
                SimEvent::Input {
                    entity: b,
                    session: json!(a),
                    input: json!({"jump": true}),
                },
                json!({"type": "input", "entity": b, "session": a, "input": {"jump": true}}),
            ),
            (
                SimEvent::Interaction {
                    entity: b,
                    kind: "hit".into(),
                    payload: json!(7),
                    from_sim: s,
                    at_tick: Some(10),
                    at: Some(Pos {
                        x: 1.0,
                        y: 2.0,
                        z: 0.0,
                    }),
                    tolerance: None,
                    rewind: Some(json!({"hit": true})),
                    seq: Some(1),
                    tick: Some(12),
                },
                json!({"type": "interaction", "entity": b, "kind": "hit", "payload": 7, "from_sim": s,
                       "at_tick": 10, "at": {"x": 1.0, "y": 2.0}, "tolerance": null,
                       "rewind": {"hit": true}, "seq": 1, "tick": 12}),
            ),
            (
                SimEvent::CoLocate {
                    entity: b,
                    with: Some(a),
                    cell: c,
                    by: s,
                    until_tick: 300,
                },
                json!({"type": "co_locate", "entity": b, "with": a, "cell": c, "by": s, "until_tick": 300}),
            ),
            (
                SimEvent::CoLocate {
                    entity: b,
                    with: None,
                    cell: c,
                    by: s,
                    until_tick: 300,
                },
                json!({"type": "co_locate", "entity": b, "cell": c, "by": s, "until_tick": 300}),
            ),
            (
                SimEvent::TxCommitted { tx: a, entity: b },
                json!({"type": "tx_committed", "tx": a, "entity": b}),
            ),
            (
                SimEvent::TxAborted {
                    tx: a,
                    entity: b,
                    reason: "held".into(),
                },
                json!({"type": "tx_aborted", "tx": a, "entity": b, "reason": "held"}),
            ),
            (
                SimEvent::NetPacket {
                    conn: 7,
                    session: json!(a),
                    entity: b,
                    data: vec![0, 255, 1],
                },
                json!({"type": "net_packet", "conn": 7, "session": a, "entity": b, "data": [0, 255, 1]}),
            ),
            (
                SimEvent::NetClose {
                    conn: 7,
                    session: json!(a),
                    entity: b,
                    reason: "rehome".into(),
                },
                json!({"type": "net_close", "conn": 7, "session": a, "entity": b, "reason": "rehome"}),
            ),
        ];
        for (ev, wire) in cases {
            assert_eq!(ev.to_value(), wire, "{ev:?}");
            let back: SimEvent<Value> = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(back, ev, "{wire}");
        }
        let newer: SimEvent<Value> =
            serde_json::from_value(json!({"type": "something_new", "x": 1})).unwrap();
        assert_eq!(newer, SimEvent::Unknown);
    }
}
