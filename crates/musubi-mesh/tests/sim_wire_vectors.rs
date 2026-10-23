//! Golden vectors for the Unreal server module (`sdk/unreal/Musubi/Source/MusubiServer`): the
//! simulator stream types as the node serialises them. Each fixture is parsed from hand-written
//! JSON, re-serialised canonically and written to `sdk/unreal/tools/sim-vectors.json`; the C++
//! side (`tools/sim-wire-test.sh`) must decode and re-encode every one to the same value. The
//! stream is JSON text only, so there is no postcard column.
use musubi_mesh::{
    sim_command::{SimCommand, SimCommandFrame},
    sim_frame::{CoLocateResult, HandoverAckResult, InteractResult, RewindResult, SimFrame},
    SimEvent,
};
use serde::{de::DeserializeOwned, Serialize};

const FRAMES: &[(&str, &str)] = &[
    (
        "tick_full",
        r#"{"type":"tick","tick":120,"cells":[{"cell":{"x":1,"y":-2},"tick":120,"owner":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","full":true,"owned":[{"id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","x":1234.5,"y":-67.25,"components":[{"name":"kind","value":"player","version":3},{"name":"hp","value":{"cur":90,"max":100},"version":7}]}],"ghosts":[{"id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d","x":5000.0,"y":5000.0,"components":[]}],"globals":["018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5e"],"time_scale":0.5}],"events":[{"type":"cell_assigned","cell":{"x":1,"y":-2},"from":null,"reason":"lease"}]}"#,
    ),
    (
        "tick_delta_gone",
        r#"{"type":"tick","tick":121,"cells":[{"cell":{"x":1,"y":-2,"z":3,"level":1},"tick":121,"owner":null,"gone":["018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d"]}]}"#,
    ),
];

const EVENTS: &[(&str, &str)] = &[
    (
        "cell_revoked",
        r#"{"type":"cell_revoked","cell":{"x":0,"y":0},"to":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","reason":"rebalance"}"#,
    ),
    (
        "cell_released",
        r#"{"type":"cell_released","cell":{"x":0,"y":0},"node":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","entities":12}"#,
    ),
    (
        "cell_reloaded",
        r#"{"type":"cell_reloaded","cell":{"x":0,"y":0},"entities":3}"#,
    ),
    (
        "cell_hibernated",
        r#"{"type":"cell_hibernated","cell":{"x":0,"y":0},"ephemeral_despawned":5,"persistent_kept":2}"#,
    ),
    (
        "net_packet",
        r#"{"type":"net_packet","conn":4503599627370495,"session":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","data":[0,128,255,7]}"#,
    ),
    (
        "net_close",
        r#"{"type":"net_close","conn":4503599627370495,"session":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","reason":"rehome"}"#,
    ),
    (
        "cell_full",
        r#"{"type":"cell_full","cell":{"x":0,"y":0},"soft_cap":500}"#,
    ),
    (
        "queued",
        r#"{"type":"queued","ticket":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","cell":{"x":0,"y":0},"position":4,"eta_ticks":90,"soft_cap":500}"#,
    ),
    (
        "queue_update",
        r#"{"type":"queue_update","ticket":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","cell":{"x":0,"y":0},"position":1,"eta_ticks":null}"#,
    ),
    (
        "queue_admitted",
        r#"{"type":"queue_admitted","ticket":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","cell":{"x":0,"y":0},"entity":{"id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","x":1.0,"y":2.0,"components":[]}}"#,
    ),
    (
        "queue_expired",
        r#"{"type":"queue_expired","ticket":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","cell":{"x":0,"y":0},"reason":"timeout"}"#,
    ),
    (
        "decayed",
        r#"{"type":"decayed","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","cell":{"x":0,"y":0}}"#,
    ),
    (
        "time_scale",
        r#"{"type":"time_scale","cell":{"x":0,"y":0},"scale":0.25}"#,
    ),
    (
        "handover_request",
        r#"{"type":"handover_request","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","from":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","state":{"id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","x":1.0,"y":2.0,"components":[{"name":"vel","value":[1.5,-2.0],"version":1}]},"group":[]}"#,
    ),
    (
        "handover_started",
        r#"{"type":"handover_started","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","to":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","node":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a60","group":["018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a61"]}"#,
    ),
    (
        "handover_completed",
        r#"{"type":"handover_completed","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","to":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b"}"#,
    ),
    (
        "handover_cancelled",
        r#"{"type":"handover_cancelled","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","reason":"refused"}"#,
    ),
    (
        "handover_arrived",
        r#"{"type":"handover_arrived","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","from_node":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a60","state":{"id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","x":1.0,"y":2.0,"components":[]},"buffered_inputs":[{"input":{"mv":[0,1]}}]}"#,
    ),
    (
        "input",
        r#"{"type":"input","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","session":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a62","input":{"mv":[1,0],"jump":true}}"#,
    ),
    (
        "interaction",
        r#"{"type":"interaction","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","kind":"hit","payload":{"dmg":10},"from_sim":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","at_tick":119,"at":{"x":1.0,"y":2.0,"z":0.5},"tolerance":1.0,"seq":7,"tick":120}"#,
    ),
    (
        "co_locate",
        r#"{"type":"co_locate","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","with":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d","cell":{"x":0,"y":0},"by":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","until_tick":420}"#,
    ),
    (
        "tx_committed",
        r#"{"type":"tx_committed","tx":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a63","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c"}"#,
    ),
    (
        "tx_aborted",
        r#"{"type":"tx_aborted","tx":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a63","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","reason":"conflict"}"#,
    ),
    (
        "unknown",
        r#"{"type":"something_new","cell":{"x":0,"y":0}}"#,
    ),
];

const COMMANDS: &[(&str, &str)] = &[
    (
        "lease",
        r#"{"op":"lease","cells":[{"x":0,"y":0},{"x":1,"y":0}]}"#,
    ),
    (
        "spawn",
        r#"{"id":1,"op":"spawn","x":10.5,"y":-3.0,"components":{"kind":"npc"},"persistent":true,"decay_ticks":600}"#,
    ),
    ("spawn_defaults", r#"{"op":"spawn","x":0.0,"y":0.0}"#),
    ("tick_done", r#"{"op":"tick_done","tick":120}"#),
    (
        "net_packet",
        r#"{"id":5,"op":"net_packet","conn":4503599627370495,"session":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","data":[0,128,255,7]}"#,
    ),
    (
        "net_close",
        r#"{"op":"net_close","conn":4503599627370495,"session":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5f","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","reason":"kicked"}"#,
    ),
    (
        "despawn",
        r#"{"id":"d-1","op":"despawn","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c"}"#,
    ),
    (
        "attach",
        r#"{"op":"attach","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","root":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d","contained":true}"#,
    ),
    (
        "detach",
        r#"{"op":"detach","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c"}"#,
    ),
    (
        "apply",
        r#"{"op":"apply","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","x":11.0,"y":-2.5,"components":{"hp":{"cur":80,"max":100}}}"#,
    ),
    (
        "interact",
        r#"{"op":"interact","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","kind":"hit","payload":{"dmg":10},"tick":120,"x":1.0,"y":2.0,"tolerance":0.5}"#,
    ),
    (
        "colocate",
        r#"{"op":"colocate","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","with":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d"}"#,
    ),
    (
        "rewind",
        r#"{"op":"rewind","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","tick":100,"x":1.0,"y":2.0}"#,
    ),
    (
        "handover_ack",
        r#"{"op":"handover_ack","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c"}"#,
    ),
];

const RESULTS: &[(&str, &str, &str)] = &[
    (
        "co_locate_result",
        "CoLocateResult",
        r#"{"pinned":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","with":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d","cell":{"x":0,"y":0},"until_tick":420,"forwarded":true,"followers":[]}"#,
    ),
    (
        "handover_ack_result",
        "HandoverAckResult",
        r#"{"outcome":{"Started":{"entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","from":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","to":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a60","group":[]}},"buffered_inputs":[{"input":{"mv":[0,1]}}]}"#,
    ),
    (
        "rewind_result",
        "RewindResult",
        r#"{"tick":100,"pos":{"x":1.0,"y":2.0},"hit":true,"history_from":60}"#,
    ),
    (
        "interact_result",
        "InteractResult",
        r#"{"delivered":"forwarded","seq":7,"tick":120,"colocated":false,"followers":["018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d"]}"#,
    ),
];

#[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
struct Vector {
    name: String,
    kind: String,
    json: String,
}

fn vectors<M: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    kind: &str,
    src: impl IntoIterator<Item = (String, String)>,
) -> Vec<Vector> {
    src.into_iter()
        .map(|(name, text)| {
            let v: M = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            let json = serde_json::to_string(&v).unwrap();
            let back: M = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v, "{name}: json roundtrip");
            Vector {
                name,
                kind: kind.into(),
                json,
            }
        })
        .collect()
}

fn owned(src: &[(&str, &str)]) -> Vec<(String, String)> {
    src.iter()
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect()
}

#[test]
fn unreal_sim_wire_vectors() {
    let mut all = vectors::<SimFrame<serde_json::Value>>("frame", owned(FRAMES));
    all.extend(vectors::<SimEvent<serde_json::Value>>(
        "event",
        owned(EVENTS),
    ));
    all.extend(vectors::<SimCommandFrame>("command", owned(COMMANDS)));
    // the plain `op` object must also parse as the bare command
    for (n, t) in COMMANDS {
        let _: SimCommand = serde_json::from_str(t).unwrap_or_else(|e| panic!("{n}: {e}"));
    }
    for (name, ty, text) in RESULTS {
        let one = std::iter::once((name.to_string(), text.to_string()));
        all.extend(match *ty {
            "CoLocateResult" => vectors::<CoLocateResult>(ty, one),
            "HandoverAckResult" => vectors::<HandoverAckResult>(ty, one),
            "RewindResult" => vectors::<RewindResult>(ty, one),
            "InteractResult" => vectors::<InteractResult>(ty, one),
            _ => unreachable!(),
        });
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../sdk/unreal/tools/sim-vectors.json"
    );
    let text = serde_json::to_string_pretty(&all).unwrap() + "\n";
    if std::env::var_os("MUSUBI_WRITE_VECTORS").is_some() {
        std::fs::write(path, text).unwrap();
        return;
    }
    let on_disk = std::fs::read_to_string(path).expect("run with MUSUBI_WRITE_VECTORS=1 once");
    assert_eq!(on_disk, text, "sim-vectors.json is stale: MUSUBI_WRITE_VECTORS=1 cargo test -p musubi-mesh --test sim_wire_vectors");
}
