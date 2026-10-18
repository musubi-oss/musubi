//! Plan 3.8 end to end: `Idl::guest_rust()` bindings compiled into a real wasm32 module and
//! driven through the runtime's ABI (rpc, match init/tick, error path). Skipped when the
//! `wasm32-unknown-unknown` target is not installed.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use musubi_runtime::{CallContext, Limits, Runtime};

const IDL: &str = r#"
package = "g"

[types.AddIn]
a = "int"
b = { type = "int", min = 0 }
tag = "uuid?"

[types.AddOut]
sum = "int"
by = "string?"

[types.ArenaParams]
rounds = { type = "int", min = 1 }

[types.ArenaState]
round = "int"
seen = "list<string>"

[rpcs.add]
input = "AddIn"
output = "AddOut"

[matches.arena]
params = "ArenaParams"
state = "ArenaState"
"#;

const HANDLERS: &str = r#"
pub struct Handlers;
mod bindings;
use bindings::*;

impl Rpcs for Handlers {
    fn add(ctx: &CallContext, input: AddIn) -> Result<AddOut, String> {
        if input.a < 0 {
            return Err(format!("a must be >= 0, got {}", input.a));
        }
        abi::log_line(2, "adding");
        Ok(AddOut { sum: input.a + input.b, by: ctx.username.clone() })
    }
}

impl ArenaMatch for Handlers {
    fn init(params: ArenaParams) -> Result<MatchInit<ArenaState>, String> {
        Ok(MatchInit { state: ArenaState { round: params.rounds, seen: vec![] }, tick_rate: 20, label: "arena".into() })
    }
    fn join_attempt(state: ArenaState, presence: &Presence, _m: &std::collections::BTreeMap<String, String>, _members: &[Presence]) -> Result<(ArenaState, Result<(), String>), String> {
        let verdict = if presence.username == "banned" { Err("banned".to_string()) } else { Ok(()) };
        Ok((state, verdict))
    }
    fn join(mut state: ArenaState, joined: &[Presence], _members: &[Presence]) -> Result<ArenaState, String> {
        state.seen.extend(joined.iter().map(|p| p.username.clone()));
        Ok(state)
    }
    fn leave(state: ArenaState, _left: &[Presence], _members: &[Presence]) -> Result<ArenaState, String> {
        Ok(state)
    }
    fn tick(mut state: ArenaState, tick: u64, inputs: Vec<Input>, _members: &[Presence]) -> Result<MatchTick<ArenaState>, String> {
        state.round += 1;
        let outbound = inputs
            .into_iter()
            .map(|i| Outbound { to: vec![i.presence.session_id], op_code: i.op_code + 1, data: i.data.iter().rev().copied().collect(), reliable: i.reliable })
            .collect();
        Ok(MatchTick { state, outbound, label: Some(format!("tick {tick}")), terminate: false })
    }
    fn terminate(state: ArenaState, _grace: u32) -> Result<ArenaState, String> {
        Ok(state)
    }
}
"#;

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [
            c[0],
            c.get(1).copied().unwrap_or(0),
            c.get(2).copied().unwrap_or(0),
        ];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(v >> 18) as usize & 63] as char);
        out.push(T[(v >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[v as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn unb64(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

/// Build the guest crate; `None` when the wasm32 target is missing on this machine.
fn build_guest() -> Option<Vec<u8>> {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown"))
        .unwrap_or(false);
    if !installed {
        eprintln!("skipping: wasm32-unknown-unknown target not installed");
        return None;
    }
    let idl = musubi_idl::Idl::parse(IDL).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/idl-guest")
        .canonicalize()
        .unwrap_or_else(|_| {
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/idl-guest");
            std::fs::create_dir_all(&p).unwrap();
            p.canonicalize().unwrap()
        });
    let crate_dir = root.join("guest");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        r#"[package]
name = "idl-guest"
version = "0.0.0"
edition = "2021"

[workspace]

[lib]
crate-type = ["cdylib"]

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["serde"] }

[profile.release]
opt-level = "s"
"#,
    )
    .unwrap();
    std::fs::write(crate_dir.join("src/lib.rs"), HANDLERS).unwrap();
    std::fs::write(crate_dir.join("src/bindings.rs"), idl.guest_rust()).unwrap();
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env_remove("RUSTFLAGS")
        .current_dir(&crate_dir)
        .output()
        .expect("cargo");
    assert!(
        out.status.success(),
        "guest build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(std::fs::read(root.join("target/wasm32-unknown-unknown/release/idl_guest.wasm")).unwrap())
}

#[test]
fn generated_bindings_speak_the_module_abi() {
    let Some(wasm) = build_guest() else { return };
    let rt = Arc::new(Runtime::new(Limits::default()).unwrap());
    let exports = rt.load_bytes("guest", &wasm).unwrap();
    assert!(exports.iter().any(|e| e == "add"), "{exports:?}");
    assert_eq!(rt.match_modules(), vec!["arena".to_string()]);
    let ctx = CallContext {
        username: Some("neo".into()),
        ..Default::default()
    };
    // typed rpc: input decoded, output encoded, context visible
    let out = rt
        .invoke_sync("rpc_add", &ctx, br#"{"a": 2, "b": 3}"#)
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v, serde_json::json!({ "sum": 5, "by": "neo" }));
    // a handler error and a malformed input both surface as call errors with the message
    let e = rt
        .invoke_sync("rpc_add", &ctx, br#"{"a": -1, "b": 3}"#)
        .unwrap_err()
        .to_string();
    assert!(e.contains("a must be >= 0"), "{e}");
    let e = rt
        .invoke_sync("rpc_add", &ctx, br#"{"a": "x"}"#)
        .unwrap_err()
        .to_string();
    assert!(e.contains("bad input"), "{e}");
    // match module: init → typed params, state as base64 JSON
    let srv = CallContext {
        server: true,
        ..Default::default()
    };
    let out = rt
        .invoke_sync("match_arena_init", &srv, br#"{"params": {"rounds": 3}}"#)
        .unwrap();
    let init: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(init["tick_rate"], 20);
    assert_eq!(init["label"], "arena");
    let state: serde_json::Value =
        serde_json::from_slice(&unb64(init["state"].as_str().unwrap())).unwrap();
    assert_eq!(state, serde_json::json!({ "round": 3, "seen": [] }));
    let pres = |name: &str| {
        serde_json::json!({
            "account_id": "00000000-0000-0000-0000-000000000001",
            "session_id": format!("sess-{name}"),
            "username": name
        })
    };
    // join_attempt verdicts
    let payload = serde_json::json!({ "state": init["state"], "presence": pres("banned"), "metadata": {}, "members": [] });
    let out = rt
        .invoke_sync(
            "match_arena_join_attempt",
            &srv,
            &serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(r["accept"], false);
    assert_eq!(r["reason"], "banned");
    // join mutates typed state
    let payload = serde_json::json!({ "state": init["state"], "presences": [pres("neo")], "members": [pres("neo")] });
    let out = rt
        .invoke_sync(
            "match_arena_join",
            &srv,
            &serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&unb64(r["state"].as_str().unwrap())).unwrap();
    assert_eq!(state["seen"], serde_json::json!(["neo"]));
    // tick: inputs decoded from base64, outbound encoded back, label and state updated
    let payload = serde_json::json!({
        "state": r["state"], "tick": 7,
        "inputs": [{ "presence": pres("neo"), "op_code": 10, "data": b64(b"abc"), "reliable": true }],
        "members": [pres("neo")]
    });
    let out = rt
        .invoke_sync(
            "match_arena_tick",
            &srv,
            &serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
    let t: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(t["label"], "tick 7");
    assert_eq!(t["terminate"], false);
    let state: serde_json::Value =
        serde_json::from_slice(&unb64(t["state"].as_str().unwrap())).unwrap();
    assert_eq!(state["round"], 4);
    let ob = &t["outbound"][0];
    assert_eq!(ob["op_code"], 11);
    assert_eq!(ob["to"], serde_json::json!(["sess-neo"]));
    assert_eq!(unb64(ob["data"].as_str().unwrap()), b"cba");
    assert_eq!(ob["reliable"], true);
    // a state that is not the declared type fails inside the guest, not silently
    let payload = serde_json::json!({ "state": b64(br#"{"round":"x"}"#), "grace_seconds": 1 });
    let e = rt
        .invoke_sync(
            "match_arena_terminate",
            &srv,
            &serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("bad state"), "{e}");
}
