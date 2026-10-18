//! Authoritative match handler backed by a WebAssembly module (`match_<module>_*` exports).
//! State crosses the boundary as base64 inside JSON, so any guest language works.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use musubi_actor::{Init, Input, MatchHandler, Outbound, TickResult};
use musubi_core::{Error, Result, SessionId};
use musubi_proto::UserPresence;
use musubi_runtime::{CallContext, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::json;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub struct WasmMatchHandler {
    runtime: Arc<Runtime>,
    module: String,
    idl: Option<Arc<parking_lot::RwLock<Option<crate::idl::IdlRegistry>>>>,
}

impl WasmMatchHandler {
    pub fn new(runtime: Arc<Runtime>, module: &str) -> Self {
        Self {
            runtime,
            module: module.to_string(),
            idl: None,
        }
    }

    /// Validate the module's state against `[matches.<module>] state` of the deployed IDL
    /// (plan 3.8): a JSON state that violates the schema fails the handler call with
    /// `Conflict` (the module and the deployed contract disagree).
    pub fn with_idl(
        mut self,
        idl: Arc<parking_lot::RwLock<Option<crate::idl::IdlRegistry>>>,
    ) -> Self {
        self.idl = Some(idl);
        self
    }

    fn state(&self, b64: &str) -> Result<Vec<u8>> {
        let state = dec(b64)?;
        let Some(idl) = &self.idl else {
            return Ok(state);
        };
        let schema = idl
            .read()
            .as_ref()
            .and_then(|r| r.match_state_schema(&self.module));
        if let Some(schema) = schema {
            if state.is_empty() {
                return Ok(state);
            }
            let v: serde_json::Value = serde_json::from_slice(&state).map_err(|e| {
                Error::Conflict(format!(
                    "match `{}` state must be JSON (declared in the IDL): {e}",
                    self.module
                ))
            })?;
            musubi_storage::schema::validate(&schema, &v).map_err(|e| {
                Error::Conflict(format!(
                    "match `{}` state violates the IDL: {e}",
                    self.module
                ))
            })?;
        }
        Ok(state)
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        part: &str,
        payload: serde_json::Value,
    ) -> Result<T> {
        let export = format!("match_{}_{part}", self.module);
        let ctx = CallContext {
            server: true,
            ..Default::default()
        };
        let out = self
            .runtime
            .invoke_sync(&export, &ctx, &serde_json::to_vec(&payload)?)?;
        serde_json::from_slice(&out)
            .map_err(|e| Error::Internal(format!("{export}: bad reply: {e}")))
    }
}

fn enc(state: &[u8]) -> String {
    B64.encode(state)
}

fn dec(s: &str) -> Result<Vec<u8>> {
    B64.decode(s)
        .map_err(|e| Error::Internal(format!("bad state base64: {e}")))
}

#[derive(Deserialize)]
struct InitReply {
    state: String,
    #[serde(default = "ten")]
    tick_rate: u32,
    #[serde(default)]
    label: String,
}

fn ten() -> u32 {
    10
}

#[derive(Deserialize)]
struct JoinAttemptReply {
    state: String,
    #[serde(default)]
    accept: bool,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct StateReply {
    state: String,
}

#[derive(Serialize)]
struct InputWire<'a> {
    presence: &'a UserPresence,
    op_code: i64,
    data: String,
    reliable: bool,
}

#[derive(Deserialize)]
struct OutboundWire {
    #[serde(default)]
    to: Vec<SessionId>,
    op_code: i64,
    #[serde(default)]
    data: String,
    #[serde(default)]
    reliable: bool,
}

#[derive(Deserialize)]
struct TickReply {
    state: String,
    #[serde(default)]
    outbound: Vec<OutboundWire>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    terminate: bool,
}

impl MatchHandler for WasmMatchHandler {
    fn init(&self, params: &serde_json::Value) -> Result<Init> {
        let r: InitReply = self.call("init", json!({ "params": params }))?;
        Ok(Init {
            state: self.state(&r.state)?,
            tick_rate: r.tick_rate,
            label: r.label,
        })
    }

    fn join_attempt(
        &self,
        state: Vec<u8>,
        presence: &UserPresence,
        metadata: &HashMap<String, String>,
        members: &[UserPresence],
    ) -> Result<(Vec<u8>, Result<()>)> {
        let r: JoinAttemptReply = self.call(
            "join_attempt",
            json!({ "state": enc(&state), "presence": presence, "metadata": metadata, "members": members }),
        )?;
        let verdict = if r.accept {
            Ok(())
        } else {
            Err(Error::PermissionDenied(
                r.reason.unwrap_or_else(|| "join rejected".into()),
            ))
        };
        Ok((self.state(&r.state)?, verdict))
    }

    fn join(
        &self,
        state: Vec<u8>,
        joined: &[UserPresence],
        members: &[UserPresence],
    ) -> Result<Vec<u8>> {
        let r: StateReply = self.call(
            "join",
            json!({ "state": enc(&state), "presences": joined, "members": members }),
        )?;
        self.state(&r.state)
    }

    fn leave(
        &self,
        state: Vec<u8>,
        left: &[UserPresence],
        members: &[UserPresence],
    ) -> Result<Vec<u8>> {
        let r: StateReply = self.call(
            "leave",
            json!({ "state": enc(&state), "presences": left, "members": members }),
        )?;
        self.state(&r.state)
    }

    fn tick(
        &self,
        state: Vec<u8>,
        tick: u64,
        inputs: Vec<Input>,
        members: &[UserPresence],
    ) -> Result<TickResult> {
        let wire: Vec<InputWire> = inputs
            .iter()
            .map(|i| InputWire {
                presence: &i.presence,
                op_code: i.op_code,
                data: enc(&i.data),
                reliable: i.reliable,
            })
            .collect();
        let r: TickReply = self.call(
            "tick",
            json!({ "state": enc(&state), "tick": tick, "inputs": wire, "members": members }),
        )?;
        let mut outbound = Vec::with_capacity(r.outbound.len());
        for o in r.outbound {
            outbound.push(Outbound {
                to: o.to,
                op_code: o.op_code,
                data: dec(&o.data)?,
                reliable: o.reliable,
            });
        }
        Ok(TickResult {
            state: self.state(&r.state)?,
            outbound,
            label: r.label,
            terminate: r.terminate,
        })
    }

    fn terminate(&self, state: Vec<u8>, grace_seconds: u32) -> Result<Vec<u8>> {
        let r: StateReply = self.call(
            "terminate",
            json!({ "state": enc(&state), "grace_seconds": grace_seconds }),
        )?;
        self.state(&r.state)
    }
}
