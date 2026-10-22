//! What the world answers a simulator with: the per-cell view of a stream `tick` frame (or
//! `GET …/view/{cell}`) and the results of `interact`, `colocate`, `rewind`, `handover_ack`.
//! Serialized exactly like the JSON the server used to assemble by hand (`sim_frame_wire`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CellId, EntityId, HandoverOutcome, Pos, SimEvent, SimId};

/// One cell as the simulator sees it. A full view (`GET …/view`, `stream` without `delta=1`)
/// carries every entity and neither `full` nor `gone`; a delta frame sets `full` (true on the
/// first frame of a cell) and lists the entities that left in `gone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellFrame<E = Value> {
    pub cell: CellId,
    pub tick: u64,
    pub owner: Option<SimId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full: Option<bool>,
    #[serde(default = "Vec::new")]
    pub owned: Vec<E>,
    #[serde(default = "Vec::new")]
    pub ghosts: Vec<E>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gone: Option<Vec<EntityId>>,
    /// Entities delivered to everyone whatever their interest.
    #[serde(default)]
    pub globals: Vec<EntityId>,
    /// Present only while the cell runs slower than real time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_scale: Option<f64>,
}

/// A frame of the simulator stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SimFrame<E = Value> {
    Tick {
        tick: u64,
        cells: Vec<CellFrame<E>>,
        #[serde(default = "Vec::new")]
        events: Vec<SimEvent<E>>,
    },
}

/// Reply to `colocate`: `pinned` sits in `cell` next to `with` until `until_tick`;
/// `followers` are its recent contact partners that came along; `forwarded` when the pin was
/// made by the node hosting the cell rather than here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoLocateResult {
    pub pinned: EntityId,
    pub with: EntityId,
    pub cell: CellId,
    pub until_tick: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forwarded: bool,
    #[serde(default)]
    pub followers: Vec<EntityId>,
}

/// Reply to `handover_ack`: how the handover ended and the inputs buffered for the entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoverAckResult {
    pub outcome: HandoverOutcome,
    #[serde(default)]
    pub buffered_inputs: Vec<Value>,
}

/// Reply to `rewind`: where the entity was at `tick` (`None` when history does not reach),
/// whether the asked position hit it, and the oldest tick the history holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewindResult {
    pub tick: u64,
    pub pos: Option<Pos>,
    pub hit: Option<bool>,
    pub history_from: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivered {
    /// Delivered to the owning simulator here (`seq` is its interaction counter).
    Owner,
    /// The entity is changing owner; the interaction follows it.
    InTransit,
    /// Sent to the node hosting the entity.
    Forwarded,
    /// Same contact pair reported twice in one tick; dropped.
    Dup,
}

/// Reply to `interact`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractResult {
    pub delivered: Delivered,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u64>,
    /// Set when the interaction was a contact: whether the pair was pinned into one cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colocated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followers: Option<Vec<EntityId>>,
}

#[cfg(test)]
mod sim_frame_wire {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn matches_the_hand_built_json() {
        let (a, b, s) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let c = CellId {
            x: 0,
            y: 0,
            z: 0,
            level: 0,
        };
        let full = CellFrame::<Value> {
            cell: c,
            tick: 5,
            owner: Some(s),
            full: None,
            owned: vec![json!({"id": a})],
            ghosts: vec![],
            gone: None,
            globals: vec![b],
            time_scale: None,
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            json!({"cell": {"x": 0, "y": 0}, "tick": 5, "owner": s, "owned": [{"id": a}], "ghosts": [], "globals": [b]})
        );
        let delta = CellFrame::<Value> {
            full: Some(false),
            gone: Some(vec![a]),
            time_scale: Some(0.5),
            owned: vec![],
            ..full.clone()
        };
        let wire = json!({"cell": {"x": 0, "y": 0}, "tick": 5, "owner": s, "full": false, "owned": [], "ghosts": [], "gone": [a], "globals": [b], "time_scale": 0.5});
        assert_eq!(serde_json::to_value(&delta).unwrap(), wire);
        assert_eq!(serde_json::from_value::<CellFrame>(wire).unwrap(), delta);
        let frame = SimFrame::Tick {
            tick: 5,
            cells: vec![full],
            events: vec![SimEvent::Decayed { entity: a, cell: c }],
        };
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "tick");
        assert_eq!(v["events"][0]["type"], "decayed");
        assert_eq!(serde_json::from_value::<SimFrame>(v).unwrap(), frame);
        assert_eq!(
            serde_json::to_value(CoLocateResult {
                pinned: a,
                with: b,
                cell: c,
                until_tick: 300,
                forwarded: false,
                followers: vec![]
            })
            .unwrap(),
            json!({"pinned": a, "with": b, "cell": {"x": 0, "y": 0}, "until_tick": 300, "followers": []})
        );
        assert_eq!(
            serde_json::to_value(RewindResult {
                tick: 7,
                pos: None,
                hit: None,
                history_from: Some(3)
            })
            .unwrap(),
            json!({"tick": 7, "pos": null, "hit": null, "history_from": 3})
        );
        assert_eq!(
            serde_json::to_value(InteractResult {
                delivered: Delivered::InTransit,
                seq: None,
                tick: None,
                colocated: Some(true),
                followers: None
            })
            .unwrap(),
            json!({"delivered": "in_transit", "colocated": true})
        );
        assert_eq!(
            serde_json::to_value(HandoverAckResult {
                outcome: HandoverOutcome::Completed { entity: a, to: s },
                buffered_inputs: vec![]
            })
            .unwrap(),
            json!({"outcome": {"Completed": {"entity": a, "to": s}}, "buffered_inputs": []})
        );
    }
}
