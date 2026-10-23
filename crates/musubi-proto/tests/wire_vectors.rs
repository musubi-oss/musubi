//! Golden vectors for the Unreal wire layer (`sdk/unreal`): every message here is parsed from
//! JSON, re-serialised to canonical JSON and postcard, and written to
//! `sdk/unreal/tools/wire-vectors.json` when `MUSUBI_WRITE_VECTORS=1`; otherwise the test asserts
//! the committed file still matches, so protocol drift fails here before it reaches the SDK.
use musubi_proto::realtime::{ClientMessage, ServerMessage};
use musubi_proto::Envelope;
use serde::{de::DeserializeOwned, Serialize};

const CLIENT: &[(&str, &str)] = &[
    ("ping", r#"{"msg":"ping"}"#),
    (
        "status_update",
        r#"{"cid":"1","msg":{"status_update":{"status":"afk"}}}"#,
    ),
    (
        "channel_join",
        r#"{"cid":"2","msg":{"channel_join":{"target":"global","kind":2,"persistence":true,"hidden":false}}}"#,
    ),
    (
        "channel_join_defaults",
        r#"{"msg":{"channel_join":{"target":"dm","kind":3}}}"#,
    ),
    (
        "matchmaker_add",
        r#"{"cid":"3","msg":{"matchmaker_add":{"min_count":2,"max_count":4,"query":"+properties.mode:ranked","string_properties":{"region":"eu"},"numeric_properties":{"mmr":1532.5},"count_multiple":2}}}"#,
    ),
    (
        "matchmaker_add_defaults",
        r#"{"msg":{"matchmaker_add":{"min_count":2,"max_count":2}}}"#,
    ),
    (
        "match_data_send",
        r#"{"msg":{"match_data_send":{"match_id":"m-1","op_code":7,"data":[1,2,3,255],"reliable":false}}}"#,
    ),
    (
        "world_subscribe",
        r#"{"cid":"4","msg":{"world_subscribe":{"world":"overworld","x":1234.5,"y":-77.25,"radius":300.0,"budget_bytes":200,"filter":["pos","hp"],"max_hz":20,"unreliable_updates":true}}}"#,
    ),
    (
        "world_input",
        r#"{"msg":{"world_input":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","input":{"move":[1,0],"jump":true}}}}"#,
    ),
    (
        "net_open",
        r#"{"cid":"9","msg":{"net_open":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b"}}}"#,
    ),
    (
        "net_packet",
        r#"{"msg":{"net_packet":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","conn":4503599627370495,"data":[0,128,255,7]}}}"#,
    ),
    (
        "net_close",
        r#"{"msg":{"net_close":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","conn":4503599627370495,"reason":"quit"}}}"#,
    ),
];

const SERVER: &[(&str, &str)] = &[
    ("pong", r#"{"msg":"pong"}"#),
    (
        "session",
        r#"{"msg":{"session":{"session_id":"s-1","resume_token":"r-1","resumed":false}}}"#,
    ),
    (
        "error",
        r#"{"cid":"3","msg":{"error":{"code":"not_found","message":"no such match"}}}"#,
    ),
    (
        "channel_message",
        r#"{"msg":{"channel_message":{"channel_id":"c-1","message_id":"m-1","kind":2,"code":0,"sender_id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","username":"neo","content":{"text":"hi","at":[1,2]},"create_time":1700000000000,"update_time":1700000000000,"persistent":true}}}"#,
    ),
    (
        "world_delta",
        r#"{"msg":{"world_delta":{"world":"overworld","tick":42,"spawned":[{"id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","x":12.34,"y":-0.02,"components":[{"name":"hp","value":{"cur":90,"max":100},"version":3}]}],"updated":[],"despawned":["018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c"],"time_scale":0.5,"sequenced":true}}}"#,
    ),
    (
        "world_delta_defaults",
        r#"{"msg":{"world_delta":{"world":"overworld","tick":43,"spawned":[],"updated":[],"despawned":[]}}}"#,
    ),
    (
        "net_opened",
        r#"{"cid":"9","msg":{"net_opened":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","conn":4503599627370495,"server":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5c","max_packet":1200}}}"#,
    ),
    (
        "net_opened_in_transit",
        r#"{"msg":{"net_opened":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","conn":1,"max_packet":1200}}}"#,
    ),
    (
        "net_packet",
        r#"{"msg":{"net_packet":{"conn":4503599627370495,"data":[1,2,3]}}}"#,
    ),
    (
        "net_close",
        r#"{"msg":{"net_close":{"conn":4503599627370495,"reason":"server shutdown"}}}"#,
    ),
    (
        "net_rehome",
        r#"{"msg":{"net_rehome":{"world":"overworld","entity":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","conn":4503599627370495,"server":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d"}}}"#,
    ),
    (
        "matchmaker_matched",
        r#"{"msg":{"matchmaker_matched":{"ticket":"t-1","token":"tok","users":[],"self":{"presence":{"account_id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5b","session_id":"018f2a3c-4b5d-7e6f-8a9b-0c1d2e3f4a5d","username":"neo","status":"","rich":false,"invisible":false},"team":1},"server":{"server_id":"gs-1","fleet_id":"eu","address":"10.0.0.1","port":7777,"connect_token":"ct"}}}}"#,
    ),
];

#[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
struct Vector {
    name: String,
    kind: String,
    json: String,
    postcard: String,
}

fn vectors<M: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    kind: &str,
    src: &[(&str, &str)],
) -> Vec<Vector> {
    src.iter()
        .map(|(name, text)| {
            let env: Envelope<M> =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            let json = serde_json::to_string(&env).unwrap();
            let back: Envelope<M> = serde_json::from_str(&json).unwrap();
            assert_eq!(back, env, "{name}: json roundtrip");
            let pc = postcard::to_allocvec(&env).unwrap();
            let back: Envelope<M> =
                postcard::from_bytes(&pc).unwrap_or_else(|e| panic!("{name}: postcard {e}"));
            assert_eq!(back, env, "{name}: postcard roundtrip");
            Vector {
                name: name.to_string(),
                kind: kind.into(),
                json,
                postcard: pc.iter().map(|b| format!("{b:02x}")).collect(),
            }
        })
        .collect()
}

#[test]
fn unreal_wire_vectors() {
    let mut all = vectors::<ClientMessage>("client", CLIENT);
    all.extend(vectors::<ServerMessage>("server", SERVER));
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../sdk/unreal/tools/wire-vectors.json"
    );
    let text = serde_json::to_string_pretty(&all).unwrap() + "\n";
    if std::env::var_os("MUSUBI_WRITE_VECTORS").is_some() {
        std::fs::write(path, text).unwrap();
        return;
    }
    let on_disk = std::fs::read_to_string(path).expect("run with MUSUBI_WRITE_VECTORS=1 once");
    assert_eq!(on_disk, text, "wire-vectors.json is stale: MUSUBI_WRITE_VECTORS=1 cargo test -p musubi-proto --test wire_vectors");
}
