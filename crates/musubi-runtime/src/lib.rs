//! Server logic runtime: sandboxed WebAssembly modules exposing RPCs.
//!
//! ABI v0 (core modules, any language that can export plain functions):
//!
//! * export `memory` and `alloc(len: i32) -> i32` (bump allocator is fine);
//! * every export named `rpc_<id>` with signature
//!   `(ctx_ptr: i32, ctx_len: i32, payload_ptr: i32, payload_len: i32) -> i64` is registered as RPC `<id>`.
//!   `ctx` is JSON (`CallContext`), `payload` is the raw request body. The return value packs the
//!   response region as `(ptr << 32) | len`; return `0` after calling `musubi.error` to fail.
//! * optional hooks with the same signature: `before_rpc_<id>` (its return value replaces the
//!   payload; call `musubi.error` to reject), `after_rpc_<id>` (receives the response, result ignored)
//!   and `hook_<event>` for server events (`authenticated`, …).
//! * match handlers: exports `match_<module>_{init,join_attempt,join,leave,tick,terminate}` with the same
//!   signature exchange JSON (state as base64); the server registers `<module>` as an authoritative
//!   match module.
//! * host imports under module `musubi`: `log(level: i32, ptr: i32, len: i32)` and
//!   `error(ptr: i32, len: i32)` (sets the error message for the current call).
//!
//! Every call runs in a fresh instance with a fuel budget, a memory cap and a wall-clock deadline,
//! so a misbehaving module can neither hang nor exhaust the node. A Component Model loader (WIT
//! `musubi:runtime`) is the next step and will sit next to this ABI.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use musubi_core::{AccountId, Error, Result, SessionId};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wasmtime::{
    Caller, Config, Engine, Linker, Module, ResourceLimiter, Store, StoreLimits,
    StoreLimitsBuilder, Val,
};

#[derive(Debug, Clone)]
pub struct Limits {
    /// Fuel per call (~1 unit per wasm instruction). 50M ≈ tens of ms of pure compute.
    pub fuel: u64,
    pub max_memory_bytes: usize,
    pub timeout: Duration,
    pub max_payload_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 50_000_000,
            max_memory_bytes: 64 << 20,
            timeout: Duration::from_secs(2),
            max_payload_bytes: 1 << 20,
        }
    }
}

/// Who is calling, as seen by the module.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallContext {
    pub account_id: Option<AccountId>,
    pub session_id: Option<SessionId>,
    pub username: Option<String>,
    /// `true` for server-to-server calls.
    pub server: bool,
    #[serde(default)]
    pub vars: serde_json::Map<String, serde_json::Value>,
}

enum Kind {
    /// Core module with the v0 ABI (`memory`/`alloc`, `rpc_<id>(ctx_ptr, ctx_len, ptr, len) -> i64`).
    Module(Module),
    /// Component Model guest implementing the `musubi:runtime/guest` world (see `wit::WIT`).
    Component(wasmtime::component::Component),
}

struct Loaded {
    name: String,
    kind: Kind,
    exports: std::collections::HashSet<String>,
}

/// The Component Model world guests target. Typed ABI: `exports()` lists the callable names
/// (same naming as the core ABI: `rpc_<id>`, `before_rpc_<id>`, `hook_*`, `match_<m>_*`), `call`
/// dispatches by name with the call context as JSON and an opaque payload.
pub mod wit {
    pub const WIT: &str = r#"
package musubi:runtime@0.1.0;

interface host {
    /// level: 0 error, 1 warn, 2 info, 3 debug
    log: func(level: u8, msg: string);
    /// JSON request `{"op": "storage.read", ...}` → JSON answer, or an error message.
    call: func(request: string) -> result<string, string>;
}

/// Typed host imports: thin wrappers over the same operations as `host.call`, so guests built with
/// wit-bindgen get real signatures instead of hand-rolled JSON. `owner`/`account` default to the
/// calling account; JSON documents travel as strings.
interface storage {
    read: func(collection: string, key: string, owner: option<string>) -> result<string, string>;
    write: func(collection: string, key: string, value: string, owner: option<string>) -> result<string, string>;
    delete: func(collection: string, key: string, owner: option<string>) -> result<_, string>;
}

interface account {
    get: func(id: option<string>) -> result<string, string>;
}

interface wallet {
    get: func(account: option<string>) -> result<string, string>;
    adjust: func(currency: string, delta: s64, reason: string, idempotency-key: option<string>, account: option<string>) -> result<string, string>;
}

interface inventory {
    get: func(account: option<string>) -> result<string, string>;
    grant: func(item-id: string, quantity: s64, account: option<string>) -> result<string, string>;
}

interface notify {
    send: func(account: string, subject: string, content: string, code: s32, persistent: bool) -> result<string, string>;
}

interface guest-api {
    exports: func() -> list<string>;
    call: func(name: string, ctx: string, payload: list<u8>) -> result<list<u8>, string>;
}

world guest {
    import host;
    import storage;
    import account;
    import wallet;
    import inventory;
    import notify;
    export guest-api;
}
"#;

    wasmtime::component::bindgen!({
        inline: r#"
package musubi:runtime@0.1.0;

interface host {
    log: func(level: u8, msg: string);
    call: func(request: string) -> result<string, string>;
}

/// Typed host imports: thin wrappers over the same operations as `host.call`, so guests built with
/// wit-bindgen get real signatures instead of hand-rolled JSON. `owner`/`account` default to the
/// calling account; JSON documents travel as strings.
interface storage {
    read: func(collection: string, key: string, owner: option<string>) -> result<string, string>;
    write: func(collection: string, key: string, value: string, owner: option<string>) -> result<string, string>;
    delete: func(collection: string, key: string, owner: option<string>) -> result<_, string>;
}

interface account {
    get: func(id: option<string>) -> result<string, string>;
}

interface wallet {
    get: func(account: option<string>) -> result<string, string>;
    adjust: func(currency: string, delta: s64, reason: string, idempotency-key: option<string>, account: option<string>) -> result<string, string>;
}

interface inventory {
    get: func(account: option<string>) -> result<string, string>;
    grant: func(item-id: string, quantity: s64, account: option<string>) -> result<string, string>;
}

interface notify {
    send: func(account: string, subject: string, content: string, code: s32, persistent: bool) -> result<string, string>;
}

interface guest-api {
    exports: func() -> list<string>;
    call: func(name: string, ctx: string, payload: list<u8>) -> result<list<u8>, string>;
}

world guest {
    import host;
    import storage;
    import account;
    import wallet;
    import inventory;
    import notify;
    export guest-api;
}
"#,
        world: "guest",
    });
}

impl State {
    /// Route a typed import through the same `HostApi` as `host.call`.
    fn op(&mut self, req: Value) -> std::result::Result<String, String> {
        match self.host.call(&self.ctx, req) {
            Ok(v) => Ok(v.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn json_or_string(s: String) -> Value {
    serde_json::from_str(&s).unwrap_or(Value::String(s))
}

impl wit::musubi::runtime::storage::Host for State {
    fn read(
        &mut self,
        collection: String,
        key: String,
        owner: Option<String>,
    ) -> std::result::Result<String, String> {
        self.op(json!({ "op": "storage.read", "collection": collection, "key": key, "owner_id": owner }))
    }

    fn write(
        &mut self,
        collection: String,
        key: String,
        value: String,
        owner: Option<String>,
    ) -> std::result::Result<String, String> {
        self.op(json!({
            "op": "storage.write", "collection": collection, "key": key,
            "value": json_or_string(value), "owner_id": owner,
        }))
    }

    fn delete(
        &mut self,
        collection: String,
        key: String,
        owner: Option<String>,
    ) -> std::result::Result<(), String> {
        self.op(json!({ "op": "storage.delete", "collection": collection, "key": key, "owner_id": owner }))
            .map(|_| ())
    }
}

impl wit::musubi::runtime::account::Host for State {
    fn get(&mut self, id: Option<String>) -> std::result::Result<String, String> {
        self.op(json!({ "op": "account.get", "account_id": id }))
    }
}

impl wit::musubi::runtime::wallet::Host for State {
    fn get(&mut self, account: Option<String>) -> std::result::Result<String, String> {
        self.op(json!({ "op": "wallet.get", "account_id": account }))
    }

    fn adjust(
        &mut self,
        currency: String,
        delta: i64,
        reason: String,
        idempotency_key: Option<String>,
        account: Option<String>,
    ) -> std::result::Result<String, String> {
        self.op(json!({
            "op": "wallet.adjust", "currency": currency, "delta": delta, "reason": reason,
            "idempotency_key": idempotency_key, "account_id": account,
        }))
    }
}

impl wit::musubi::runtime::inventory::Host for State {
    fn get(&mut self, account: Option<String>) -> std::result::Result<String, String> {
        self.op(json!({ "op": "inventory.get", "account_id": account }))
    }

    fn grant(
        &mut self,
        item_id: String,
        quantity: i64,
        account: Option<String>,
    ) -> std::result::Result<String, String> {
        self.op(json!({
            "op": "inventory.grant", "item_id": item_id, "quantity": quantity, "account_id": account,
        }))
    }
}

impl wit::musubi::runtime::notify::Host for State {
    fn send(
        &mut self,
        account: String,
        subject: String,
        content: String,
        code: i32,
        persistent: bool,
    ) -> std::result::Result<String, String> {
        self.op(json!({
            "op": "notify.send", "account_id": account, "subject": subject,
            "content": json_or_string(content), "code": code, "persistent": persistent,
        }))
    }
}

impl wit::musubi::runtime::host::Host for State {
    fn call(&mut self, request: String) -> std::result::Result<String, String> {
        match serde_json::from_str::<Value>(&request)
            .map_err(|e| Error::InvalidArgument(format!("host request is not json: {e}")))
            .and_then(|req| self.host.call(&self.ctx, req))
        {
            Ok(v) => Ok(v.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn log(&mut self, level: u8, msg: String) {
        let module = self.module.clone();
        match level {
            0 => tracing::error!(module, "{msg}"),
            1 => tracing::warn!(module, "{msg}"),
            2 => tracing::info!(module, "{msg}"),
            _ => tracing::debug!(module, "{msg}"),
        }
    }
}

/// What guests may ask the server to do (`musubi.host_call` / `host.call`): a JSON request with
/// an `op` and op-specific fields, answered with JSON. The server installs its implementation;
/// without one every call fails with `Unavailable`.
pub trait HostApi: Send + Sync {
    fn call(&self, ctx: &CallContext, request: Value) -> Result<Value>;
}

struct NoHost;

impl HostApi for NoHost {
    fn call(&self, _ctx: &CallContext, _request: Value) -> Result<Value> {
        Err(Error::Unavailable("runtime host api not installed".into()))
    }
}

struct State {
    limits: StoreLimits,
    error: Option<String>,
    module: String,
    ctx: CallContext,
    host: Arc<dyn HostApi>,
}

/// Run a host request and encode the answer the way guests expect.
fn host_call(state: &State, request: &[u8]) -> Vec<u8> {
    let out = serde_json::from_slice::<Value>(request)
        .map_err(|e| Error::InvalidArgument(format!("host request is not json: {e}")))
        .and_then(|req| state.host.call(&state.ctx, req));
    let v = match out {
        Ok(v) => serde_json::json!({ "ok": v }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    };
    serde_json::to_vec(&v).unwrap_or_default()
}

impl ResourceLimiter for State {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.memory_growing(current, desired, maximum)
    }
    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }
}

pub struct Runtime {
    engine: Engine,
    linker: Linker<State>,
    component_linker: wasmtime::component::Linker<State>,
    host: RwLock<Arc<dyn HostApi>>,
    limits: Limits,
    /// rpc id → module exporting it
    rpcs: RwLock<HashMap<String, Arc<Loaded>>>,
    /// hook export name → module
    hooks: RwLock<HashMap<String, Arc<Loaded>>>,
    /// every function export → module (for `invoke_sync`)
    exports: RwLock<HashMap<String, Arc<Loaded>>>,
    modules: RwLock<HashMap<String, Arc<Loaded>>>,
}

impl Runtime {
    pub fn new(limits: Limits) -> Result<Self> {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        cfg.wasm_backtrace(false);
        let engine = Engine::new(&cfg).map_err(Error::internal)?;
        let mut linker: Linker<State> = Linker::new(&engine);
        linker
            .func_wrap(
                "musubi",
                "log",
                |mut caller: Caller<'_, State>, level: i32, ptr: i32, len: i32| {
                    let msg = read_string(&mut caller, ptr, len).unwrap_or_default();
                    let module = caller.data().module.clone();
                    match level {
                        0 => tracing::error!(module, "{msg}"),
                        1 => tracing::warn!(module, "{msg}"),
                        2 => tracing::info!(module, "{msg}"),
                        _ => tracing::debug!(module, "{msg}"),
                    }
                },
            )
            .map_err(Error::internal)?;
        linker
            .func_wrap(
                "musubi",
                "error",
                |mut caller: Caller<'_, State>, ptr: i32, len: i32| {
                    let msg = read_string(&mut caller, ptr, len)
                        .unwrap_or_else(|_| "module error".into());
                    caller.data_mut().error = Some(msg);
                },
            )
            .map_err(Error::internal)?;
        linker
            .func_wrap(
                "musubi",
                "host_call",
                |mut caller: Caller<'_, State>, ptr: i32, len: i32| -> wasmtime::Result<i64> {
                    let req = read_bytes(&mut caller, ptr, len)?;
                    let out = host_call(caller.data(), &req);
                    let memory = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .ok_or_else(|| wasmtime::Error::msg("no memory"))?;
                    let alloc = caller
                        .get_export("alloc")
                        .and_then(|e| e.into_func())
                        .ok_or_else(|| wasmtime::Error::msg("no alloc"))?
                        .typed::<i32, i32>(&caller)?;
                    let out_ptr = alloc.call(&mut caller, out.len() as i32)?;
                    let needed = out_ptr as usize + out.len();
                    let have = memory.data_size(&caller);
                    if needed > have {
                        memory.grow(&mut caller, (needed - have).div_ceil(65536) as u64)?;
                    }
                    memory.write(&mut caller, out_ptr as usize, &out)?;
                    Ok(((out_ptr as u64) << 32 | out.len() as u64) as i64)
                },
            )
            .map_err(Error::internal)?;
        // Epoch ticker: one tick = 10 ms; deadlines are set per call.
        let ticker = engine.clone();
        std::thread::Builder::new()
            .name("musubi-runtime-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(10));
                ticker.increment_epoch();
            })
            .map_err(Error::internal)?;
        let mut component_linker = wasmtime::component::Linker::new(&engine);
        wit::Guest::add_to_linker(&mut component_linker, |s: &mut State| s)
            .map_err(Error::internal)?;
        Ok(Self {
            engine,
            linker,
            component_linker,
            host: RwLock::new(Arc::new(NoHost)),
            limits,
            rpcs: RwLock::new(HashMap::new()),
            hooks: RwLock::new(HashMap::new()),
            exports: RwLock::new(HashMap::new()),
            modules: RwLock::new(HashMap::new()),
        })
    }

    /// Install the server side of `musubi.host_call` / `host.call`.
    pub fn set_host(&self, host: Arc<dyn HostApi>) {
        *self.host.write() = host;
    }

    /// Compile and register a module (`.wasm` or `.wat`). Re-loading a name replaces its RPCs (hot reload).
    pub fn load_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<String>> {
        let (kind, names) = if is_component(bytes) {
            let component = wasmtime::component::Component::new(&self.engine, bytes)
                .map_err(|e| Error::InvalidArgument(format!("component {name}: {e}")))?;
            let names = self.component_exports(name, &component)?;
            (Kind::Component(component), names)
        } else {
            let module = Module::new(&self.engine, bytes)
                .map_err(|e| Error::InvalidArgument(format!("module {name}: {e}")))?;
            let names: Vec<String> = module.exports().map(|e| e.name().to_string()).collect();
            if !names.iter().any(|n| n == "alloc") || !names.iter().any(|n| n == "memory") {
                return Err(Error::InvalidArgument(format!(
                    "module {name}: must export `memory` and `alloc`"
                )));
            }
            (Kind::Module(module), names)
        };
        let mut ids = Vec::new();
        let mut hooks = Vec::new();
        let mut exports = std::collections::HashSet::new();
        for n in names {
            if let Some(id) = n.strip_prefix("rpc_") {
                ids.push(id.to_string());
            } else if n.starts_with("before_rpc_")
                || n.starts_with("after_rpc_")
                || n.starts_with("hook_")
                || n == "before_chat_message"
            {
                hooks.push(n.clone());
            }
            exports.insert(n);
        }
        let loaded = Arc::new(Loaded {
            name: name.to_string(),
            kind,
            exports,
        });
        let mut rpcs = self.rpcs.write();
        let mut hooks_map = self.hooks.write();
        let mut exports_map = self.exports.write();
        let mut modules = self.modules.write();
        if let Some(old) = modules.insert(name.to_string(), loaded.clone()) {
            rpcs.retain(|_, m| !Arc::ptr_eq(m, &old));
            hooks_map.retain(|_, m| !Arc::ptr_eq(m, &old));
            exports_map.retain(|_, m| !Arc::ptr_eq(m, &old));
        }
        for e in &loaded.exports {
            exports_map.insert(e.clone(), loaded.clone());
        }
        for id in &ids {
            rpcs.insert(id.clone(), loaded.clone());
        }
        for h in &hooks {
            hooks_map.insert(h.clone(), loaded.clone());
        }
        tracing::info!(module = name, rpcs = ?ids, "runtime module loaded");
        Ok(ids)
    }

    /// Load every `*.wasm` / `*.wat` in a directory. Returns (modules, rpcs).
    pub fn load_dir(&self, dir: &Path) -> Result<(usize, usize)> {
        let mut modules = 0;
        let mut rpcs = 0;
        let entries = std::fs::read_dir(dir)
            .map_err(|e| Error::InvalidArgument(format!("{}: {e}", dir.display())))?;
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "wasm" && ext != "wat" {
                continue;
            }
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("module")
                .to_string();
            let bytes = std::fs::read(&p).map_err(Error::internal)?;
            rpcs += self.load_bytes(&name, &bytes)?.len();
            modules += 1;
        }
        Ok((modules, rpcs))
    }

    /// Drop a module and every RPC/hook/export it provided. Returns whether it was loaded.
    pub fn unload(&self, name: &str) -> bool {
        let mut rpcs = self.rpcs.write();
        let mut hooks_map = self.hooks.write();
        let mut exports_map = self.exports.write();
        let mut modules = self.modules.write();
        let Some(old) = modules.remove(name) else {
            return false;
        };
        rpcs.retain(|_, m| !Arc::ptr_eq(m, &old));
        hooks_map.retain(|_, m| !Arc::ptr_eq(m, &old));
        exports_map.retain(|_, m| !Arc::ptr_eq(m, &old));
        tracing::info!(module = name, "runtime module unloaded");
        true
    }

    pub fn rpc_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.rpcs.read().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn module_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.modules.read().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn has_rpc(&self, id: &str) -> bool {
        self.rpcs.read().contains_key(id)
    }

    /// Invoke an RPC off the async runtime (wasm execution is blocking). Runs `before_rpc_<id>` /
    /// `after_rpc_<id>` hooks when a module exports them.
    pub async fn call(
        self: &Arc<Self>,
        id: &str,
        ctx: CallContext,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>> {
        if payload.len() > self.limits.max_payload_bytes {
            return Err(Error::InvalidArgument("rpc payload too large".into()));
        }
        let loaded = self
            .rpcs
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("rpc {id}")))?;
        let payload = match self
            .hook(&format!("before_rpc_{id}"), ctx.clone(), payload.clone())
            .await
        {
            Some(Ok(rewritten)) => rewritten,
            Some(Err(e)) => return Err(e),
            None => payload,
        };
        let out = self
            .invoke(loaded, &format!("rpc_{id}"), ctx.clone(), payload)
            .await?;
        if let Some(Err(e)) = self
            .hook(&format!("after_rpc_{id}"), ctx, out.clone())
            .await
        {
            tracing::warn!(rpc = id, error = %e, "after hook failed");
        }
        Ok(out)
    }

    /// Run a hook export if any module provides it. `None` when nobody listens.
    pub async fn hook(
        self: &Arc<Self>,
        name: &str,
        ctx: CallContext,
        payload: Vec<u8>,
    ) -> Option<Result<Vec<u8>>> {
        let loaded = self.hooks.read().get(name).cloned()?;
        Some(self.invoke(loaded, name, ctx, payload).await)
    }

    /// Fire a server event (`hook_<event>`) with a JSON payload; errors are logged, not propagated.
    pub async fn event(
        self: &Arc<Self>,
        event: &str,
        ctx: CallContext,
        payload: serde_json::Value,
    ) {
        let name = format!("hook_{event}");
        let bytes = serde_json::to_vec(&payload).unwrap_or_default();
        if let Some(Err(e)) = self.hook(&name, ctx, bytes).await {
            tracing::warn!(hook = name, error = %e, "hook failed");
        }
    }

    /// Match modules: every `<m>` with a complete `match_<m>_*` export set.
    pub fn match_modules(&self) -> Vec<String> {
        const PARTS: [&str; 6] = ["init", "join_attempt", "join", "leave", "tick", "terminate"];
        let ex = self.exports.read();
        let mut v: Vec<String> = ex
            .keys()
            .filter_map(|k| {
                k.strip_prefix("match_")?
                    .strip_suffix("_init")
                    .map(str::to_string)
            })
            .filter(|m| {
                PARTS
                    .iter()
                    .all(|p| ex.contains_key(&format!("match_{m}_{p}")))
            })
            .collect();
        v.sort();
        v
    }

    /// Synchronous invoke of any export (used by match actors on their own task).
    pub fn invoke_sync(&self, export: &str, ctx: &CallContext, payload: &[u8]) -> Result<Vec<u8>> {
        let loaded = self
            .exports
            .read()
            .get(export)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("export {export}")))?;
        self.call_blocking(&loaded, export, ctx, payload)
    }

    pub fn has_hook(&self, name: &str) -> bool {
        self.hooks.read().contains_key(name)
    }

    async fn invoke(
        self: &Arc<Self>,
        loaded: Arc<Loaded>,
        export: &str,
        ctx: CallContext,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>> {
        let this = self.clone();
        let export = export.to_string();
        tokio::task::spawn_blocking(move || this.call_blocking(&loaded, &export, &ctx, &payload))
            .await
            .map_err(Error::internal)?
    }

    fn new_store(&self, module: &str, ctx: &CallContext) -> Result<Store<State>> {
        let state = State {
            limits: StoreLimitsBuilder::new()
                .memory_size(self.limits.max_memory_bytes)
                .instances(1)
                .build(),
            error: None,
            module: module.to_string(),
            ctx: ctx.clone(),
            host: self.host.read().clone(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| s);
        store.set_fuel(self.limits.fuel).map_err(Error::internal)?;
        let ticks = (self.limits.timeout.as_millis() / 10).max(1) as u64;
        store.set_epoch_deadline(ticks);
        Ok(store)
    }

    fn trap_error(id: &str, store: &mut Store<State>, trap: wasmtime::Error) -> Error {
        if let Some(msg) = store.data_mut().error.take() {
            return Error::InvalidArgument(msg);
        }
        let text = trap.to_string();
        if text.contains("fuel") {
            Error::ResourceExhausted(format!("rpc {id}: fuel exhausted"))
        } else if text.contains("epoch") || text.contains("interrupt") {
            Error::ResourceExhausted(format!("rpc {id}: timed out"))
        } else {
            Error::Internal(format!(
                "rpc {id}: {}",
                text.lines().next().unwrap_or("trap")
            ))
        }
    }

    /// Ask a component which names it serves (one throwaway instance under the usual limits).
    fn component_exports(
        &self,
        name: &str,
        component: &wasmtime::component::Component,
    ) -> Result<Vec<String>> {
        let mut store = self.new_store(name, &CallContext::default())?;
        let guest = wit::Guest::instantiate(&mut store, component, &self.component_linker)
            .map_err(|e| Error::InvalidArgument(format!("component {name}: {e}")))?;
        guest
            .musubi_runtime_guest_api()
            .call_exports(&mut store)
            .map_err(|e| Self::trap_error("exports", &mut store, e))
    }

    fn call_component(
        &self,
        loaded: &Loaded,
        component: &wasmtime::component::Component,
        export: &str,
        ctx: &CallContext,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        let mut store = self.new_store(&loaded.name, ctx)?;
        let guest = wit::Guest::instantiate(&mut store, component, &self.component_linker)
            .map_err(|e| Error::Internal(format!("instantiate {}: {e}", loaded.name)))?;
        let ctx_json = serde_json::to_string(ctx)?;
        match guest
            .musubi_runtime_guest_api()
            .call_call(&mut store, export, &ctx_json, payload)
        {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(msg)) => Err(Error::InvalidArgument(msg)),
            Err(trap) => Err(Self::trap_error(export, &mut store, trap)),
        }
    }

    fn call_blocking(
        &self,
        loaded: &Loaded,
        export: &str,
        ctx: &CallContext,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        if !loaded.exports.contains(export) {
            return Err(Error::NotFound(format!("export {export}")));
        }
        let module = match &loaded.kind {
            Kind::Component(c) => return self.call_component(loaded, c, export, ctx, payload),
            Kind::Module(m) => m,
        };
        let id = export;
        let mut store = self.new_store(&loaded.name, ctx)?;
        let instance = self
            .linker
            .instantiate(&mut store, module)
            .map_err(|e| Error::Internal(format!("instantiate {}: {e}", loaded.name)))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| Error::Internal("no memory".into()))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|e| Error::Internal(format!("alloc: {e}")))?;
        let func = instance
            .get_func(&mut store, export)
            .ok_or_else(|| Error::NotFound(format!("export {export}")))?;

        let ctx_bytes = serde_json::to_vec(ctx)?;
        let ctx_ptr = write_guest(&mut store, &memory, &alloc, &ctx_bytes)?;
        let payload_ptr = write_guest(&mut store, &memory, &alloc, payload)?;
        let args = [
            Val::I32(ctx_ptr),
            Val::I32(ctx_bytes.len() as i32),
            Val::I32(payload_ptr),
            Val::I32(payload.len() as i32),
        ];
        let mut results = [Val::I64(0)];
        if let Err(trap) = func.call(&mut store, &args, &mut results) {
            return Err(Self::trap_error(id, &mut store, trap));
        }
        if let Some(msg) = store.data_mut().error.take() {
            return Err(Error::InvalidArgument(msg));
        }
        let packed = match results[0] {
            Val::I64(v) => v as u64,
            _ => return Err(Error::Internal("rpc returned a non-i64".into())),
        };
        let (ptr, len) = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
        if len == 0 {
            return Ok(Vec::new());
        }
        let data = memory.data(&store);
        let end = ptr
            .checked_add(len)
            .filter(|e| *e <= data.len())
            .ok_or_else(|| Error::Internal("rpc returned an out-of-bounds region".into()))?;
        Ok(data[ptr..end].to_vec())
    }
}

/// Component binaries carry layer `0x01` in the version field; text form starts with `(component`.
fn is_component(bytes: &[u8]) -> bool {
    if bytes.len() >= 8 && &bytes[..4] == b"\0asm" {
        return bytes[6] == 1 && bytes[7] == 0;
    }
    std::str::from_utf8(bytes)
        .map(|t| {
            t.trim_start()
                .trim_start_matches([';', ' '])
                .starts_with("(component")
        })
        .unwrap_or(false)
}

fn write_guest(
    store: &mut Store<State>,
    memory: &wasmtime::Memory,
    alloc: &wasmtime::TypedFunc<i32, i32>,
    bytes: &[u8],
) -> Result<i32> {
    let ptr = alloc
        .call(&mut *store, bytes.len() as i32)
        .map_err(|e| Error::Internal(format!("alloc: {e}")))?;
    let needed = ptr as usize + bytes.len();
    let have = memory.data_size(&*store);
    if needed > have {
        let pages = (needed - have).div_ceil(65536) as u64;
        memory
            .grow(&mut *store, pages)
            .map_err(|e| Error::ResourceExhausted(format!("memory: {e}")))?;
    }
    memory
        .write(&mut *store, ptr as usize, bytes)
        .map_err(|e| Error::Internal(format!("memory write: {e}")))?;
    Ok(ptr)
}

fn read_bytes(caller: &mut Caller<'_, State>, ptr: i32, len: i32) -> wasmtime::Result<Vec<u8>> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("no memory"))?;
    let data = memory.data(caller);
    let (ptr, len) = (ptr as usize, len as usize);
    let end = ptr
        .checked_add(len)
        .filter(|e| *e <= data.len())
        .ok_or_else(|| wasmtime::Error::msg("host_call: out-of-bounds request"))?;
    Ok(data[ptr..end].to_vec())
}

fn read_string(caller: &mut Caller<'_, State>, ptr: i32, len: i32) -> Result<String> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| Error::Internal("no memory".into()))?;
    let data = memory.data(caller);
    let (ptr, len) = (ptr as usize, len as usize);
    let end = ptr
        .checked_add(len)
        .filter(|e| *e <= data.len())
        .ok_or_else(|| Error::Internal("oob".into()))?;
    Ok(String::from_utf8_lossy(&data[ptr..end]).into_owned())
}

/// A tiny module used by tests and as the smallest possible ABI example.
pub const EXAMPLE_WAT: &str = r#"
(module
  (import "musubi" "error" (func $error (param i32 i32)))
  (import "musubi" "log" (func $log (param i32 i32 i32)))
  (import "musubi" "host_call" (func $host_call (param i32 i32) (result i64)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
  (func (export "alloc") (param $n i32) (result i32)
    (local $p i32)
    global.get $heap
    local.set $p
    global.get $heap
    local.get $n
    i32.add
    global.set $heap
    local.get $p)
  ;; echo: return the payload untouched
  (func (export "rpc_echo") (param i32 i32 i32 i32) (result i64)
    i32.const 2 local.get 2 local.get 3 call $log
    local.get 2 i64.extend_i32_u i64.const 32 i64.shl
    local.get 3 i64.extend_i32_u i64.or)
  ;; host: forward the payload to the server as a host request and return its answer
  (func (export "rpc_host") (param i32 i32 i32 i32) (result i64)
    local.get 2 local.get 3 call $host_call)
  ;; whoami: return the context JSON
  (func (export "rpc_whoami") (param i32 i32 i32 i32) (result i64)
    local.get 0 i64.extend_i32_u i64.const 32 i64.shl
    local.get 1 i64.extend_i32_u i64.or)
  ;; echo2 is echo with a before-hook that rewrites the payload and an after-hook that logs
  (func (export "rpc_echo2") (param i32 i32 i32 i32) (result i64)
    local.get 2 i64.extend_i32_u i64.const 32 i64.shl
    local.get 3 i64.extend_i32_u i64.or)
  (data (i32.const 16) "{\"hooked\":true}")
  (func (export "before_rpc_echo2") (param i32 i32 i32 i32) (result i64)
    i64.const 16 i64.const 32 i64.shl i64.const 15 i64.or)
  (func (export "after_rpc_echo2") (param i32 i32 i32 i32) (result i64)
    i32.const 2 local.get 2 local.get 3 call $log
    i64.const 0)
  (func (export "hook_authenticated") (param i32 i32 i32 i32) (result i64)
    i32.const 2 local.get 2 local.get 3 call $log
    i64.const 0)
  ;; demo match module: constant JSON replies (state stays empty)
  (data (i32.const 64) "{\"state\":\"\",\"tick_rate\":10,\"label\":\"demo\"}")
  (func (export "match_demo_init") (param i32 i32 i32 i32) (result i64)
    i64.const 64 i64.const 32 i64.shl i64.const 42 i64.or)
  (data (i32.const 128) "{\"state\":\"\",\"accept\":true}")
  (func (export "match_demo_join_attempt") (param i32 i32 i32 i32) (result i64)
    i64.const 128 i64.const 32 i64.shl i64.const 26 i64.or)
  (data (i32.const 192) "{\"state\":\"\"}")
  (func (export "match_demo_join") (param i32 i32 i32 i32) (result i64)
    i64.const 192 i64.const 32 i64.shl i64.const 12 i64.or)
  (func (export "match_demo_leave") (param i32 i32 i32 i32) (result i64)
    i64.const 192 i64.const 32 i64.shl i64.const 12 i64.or)
  (func (export "match_demo_terminate") (param i32 i32 i32 i32) (result i64)
    i64.const 192 i64.const 32 i64.shl i64.const 12 i64.or)
  (data (i32.const 256) "{\"state\":\"\",\"outbound\":[],\"terminate\":false}")
  (func (export "match_demo_tick") (param i32 i32 i32 i32) (result i64)
    i64.const 256 i64.const 32 i64.shl i64.const 44 i64.or)
  (data (i32.const 0) "boom")
  (func (export "rpc_fail") (param i32 i32 i32 i32) (result i64)
    i32.const 0 i32.const 4 call $error
    i64.const 0)
  (func (export "rpc_spin") (param i32 i32 i32 i32) (result i64)
    (loop br 0)
    i64.const 0)
)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_fail_and_fuel() {
        let rt = Arc::new(
            Runtime::new(Limits {
                fuel: 1_000_000,
                ..Default::default()
            })
            .unwrap(),
        );
        let ids = rt.load_bytes("example", EXAMPLE_WAT.as_bytes()).unwrap();
        assert_eq!(ids, vec!["echo", "host", "whoami", "echo2", "fail", "spin"]);
        let hooked = rt
            .call("echo2", CallContext::default(), b"ignored".to_vec())
            .await
            .unwrap();
        assert_eq!(hooked, br#"{"hooked":true}"#);
        assert!(rt.has_hook("hook_authenticated"));
        assert_eq!(rt.match_modules(), vec!["demo"]);
        let init = rt
            .invoke_sync("match_demo_init", &CallContext::default(), b"{}")
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&init).unwrap()["label"],
            "demo"
        );
        assert!(rt
            .hook("hook_nope", CallContext::default(), vec![])
            .await
            .is_none());
        rt.event(
            "authenticated",
            CallContext::default(),
            serde_json::json!({"created": true}),
        )
        .await;
        let out = rt
            .call("echo", CallContext::default(), b"{\"hi\":1}".to_vec())
            .await
            .unwrap();
        assert_eq!(out, b"{\"hi\":1}");
        let ctx = CallContext {
            username: Some("alice".into()),
            ..Default::default()
        };
        let who = rt.call("whoami", ctx, vec![]).await.unwrap();
        assert!(String::from_utf8(who).unwrap().contains("alice"));
        match rt.call("fail", CallContext::default(), vec![]).await {
            Err(Error::InvalidArgument(m)) => assert_eq!(m, "boom"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            rt.call("spin", CallContext::default(), vec![]).await,
            Err(Error::ResourceExhausted(_))
        ));
        assert!(matches!(
            rt.call("nope", CallContext::default(), vec![]).await,
            Err(Error::NotFound(_))
        ));
        // hot reload replaces the RPC set
        rt.load_bytes(
            "example",
            EXAMPLE_WAT.replace("rpc_spin", "rpc_loop").as_bytes(),
        )
        .unwrap();
        assert!(!rt.has_rpc("spin") && rt.has_rpc("loop"));
    }
}

#[cfg(test)]
mod component_tests {
    use super::*;

    /// Hand-written component: `exports()` → ["rpc_echo", "rpc_err"], `call` echoes the payload
    /// for names of length 8 and returns `Err("boom")` for `rpc_err`.
    const COMPONENT: &str = r#"
(component
  (core module $M
    (memory (export "memory") 1)
    (global $heap (mut i32) (i32.const 4096))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (global.get $heap))
      (global.set $heap (i32.add (global.get $heap) (local.get 3)))
      (local.get $p))
    (data (i32.const 16) "rpc_echo")
    (data (i32.const 32) "rpc_err")
    (data (i32.const 48) "boom")
    (func (export "exports") (result i32)
      (i32.store (i32.const 64) (i32.const 16)) (i32.store (i32.const 68) (i32.const 8))
      (i32.store (i32.const 72) (i32.const 32)) (i32.store (i32.const 76) (i32.const 7))
      (i32.store (i32.const 96) (i32.const 64)) (i32.store (i32.const 100) (i32.const 2))
      (i32.const 96))
    (func (export "call") (param i32 i32 i32 i32 i32 i32) (result i32)
      (if (i32.eq (local.get 1) (i32.const 7))
        (then
          (i32.store (i32.const 128) (i32.const 1))
          (i32.store (i32.const 132) (i32.const 48))
          (i32.store (i32.const 136) (i32.const 4)))
        (else
          (i32.store (i32.const 128) (i32.const 0))
          (i32.store (i32.const 132) (local.get 4))
          (i32.store (i32.const 136) (local.get 5))))
      (i32.const 128))
  )
  (core instance $m (instantiate $M))
  (func $exports (result (list string))
    (canon lift (core func $m "exports") (memory (core memory $m "memory")) (realloc (core func $m "realloc")) string-encoding=utf8))
  (func $call (param "name" string) (param "ctx" string) (param "payload" (list u8))
    (result (result (list u8) (error string)))
    (canon lift (core func $m "call") (memory (core memory $m "memory")) (realloc (core func $m "realloc")) string-encoding=utf8))
  (instance $api (export "exports" (func $exports)) (export "call" (func $call)))
  (export "musubi:runtime/guest-api@0.1.0" (instance $api))
)
"#;

    #[tokio::test]
    async fn component_guest_lists_exports_and_dispatches_calls() {
        let rt = Arc::new(Runtime::new(Limits::default()).unwrap());
        let ids = rt.load_bytes("demo", COMPONENT.as_bytes()).unwrap();
        assert_eq!(ids, vec!["echo".to_string(), "err".to_string()]);
        assert!(rt.has_rpc("echo"));
        let ctx = CallContext::default();
        let out = rt
            .call("echo", ctx.clone(), b"hello".to_vec())
            .await
            .unwrap();
        assert_eq!(out, b"hello");
        let err = rt.call("err", ctx, b"x".to_vec()).await.unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument(m) if m == "boom"),
            "{err:?}"
        );
        assert!(is_component(COMPONENT.as_bytes()));
        assert!(!is_component(b"(module)"));
    }

    /// Component using the typed `storage` imports: `rpc_kv` writes the payload under c/k and
    /// returns what `read` gives back. Two core modules because a lowered import that returns a
    /// string needs the instance's memory/realloc, which cannot come from the importing module.
    const TYPED_COMPONENT: &str = r#"
(component
  (import "musubi:runtime/storage@0.1.0" (instance $storage
    (export "write" (func (param "collection" string) (param "key" string) (param "value" string)
      (param "owner" (option string)) (result (result string (error string)))))
    (export "read" (func (param "collection" string) (param "key" string)
      (param "owner" (option string)) (result (result string (error string)))))))
  (core module $M
    (memory (export "memory") 1)
    (global $heap (mut i32) (i32.const 4096))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (global.get $heap))
      (global.set $heap (i32.add (global.get $heap) (local.get 3)))
      (local.get $p)))
  (core instance $m (instantiate $M))
  (core func $write_l (canon lower (func $storage "write")
    (memory (core memory $m "memory")) (realloc (core func $m "realloc")) string-encoding=utf8))
  (core func $read_l (canon lower (func $storage "read")
    (memory (core memory $m "memory")) (realloc (core func $m "realloc")) string-encoding=utf8))
  (core module $N
    (import "env" "memory" (memory 1))
    (import "storage" "write" (func $write (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
    (import "storage" "read" (func $read (param i32 i32 i32 i32 i32 i32 i32 i32)))
    (data (i32.const 16) "rpc_kv")
    (data (i32.const 32) "c")
    (data (i32.const 40) "k")
    (func (export "exports") (result i32)
      (i32.store (i32.const 64) (i32.const 16)) (i32.store (i32.const 68) (i32.const 6))
      (i32.store (i32.const 96) (i32.const 64)) (i32.store (i32.const 100) (i32.const 1))
      (i32.const 96))
    (func (export "call") (param i32 i32 i32 i32 i32 i32) (result i32)
      (call $write (i32.const 32) (i32.const 1) (i32.const 40) (i32.const 1)
        (local.get 4) (local.get 5) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 200))
      (if (i32.ne (i32.load (i32.const 200)) (i32.const 0))
        (then
          (i32.store (i32.const 128) (i32.const 1))
          (i32.store (i32.const 132) (i32.load (i32.const 204)))
          (i32.store (i32.const 136) (i32.load (i32.const 208)))
          (return (i32.const 128))))
      (call $read (i32.const 32) (i32.const 1) (i32.const 40) (i32.const 1)
        (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 240))
      (i32.store (i32.const 128) (i32.load (i32.const 240)))
      (i32.store (i32.const 132) (i32.load (i32.const 244)))
      (i32.store (i32.const 136) (i32.load (i32.const 248)))
      (i32.const 128)))
  (core instance $n (instantiate $N
    (with "env" (instance (export "memory" (memory $m "memory"))))
    (with "storage" (instance (export "write" (func $write_l)) (export "read" (func $read_l))))))
  (func $exports (result (list string))
    (canon lift (core func $n "exports") (memory (core memory $m "memory")) (realloc (core func $m "realloc")) string-encoding=utf8))
  (func $call (param "name" string) (param "ctx" string) (param "payload" (list u8))
    (result (result (list u8) (error string)))
    (canon lift (core func $n "call") (memory (core memory $m "memory")) (realloc (core func $m "realloc")) string-encoding=utf8))
  (instance $api (export "exports" (func $exports)) (export "call" (func $call)))
  (export "musubi:runtime/guest-api@0.1.0" (instance $api))
)
"#;

    /// In-memory `HostApi` that records requests and serves `storage.read` from its own map.
    #[derive(Default)]
    struct MemHost {
        calls: parking_lot::Mutex<Vec<Value>>,
        kv: parking_lot::Mutex<HashMap<String, Value>>,
    }

    impl HostApi for MemHost {
        fn call(&self, _ctx: &CallContext, request: Value) -> Result<Value> {
            self.calls.lock().push(request.clone());
            let k = format!("{}/{}", request["collection"], request["key"]);
            match request["op"].as_str() {
                Some("storage.write") => {
                    self.kv.lock().insert(k, request["value"].clone());
                    Ok(json!({ "version": "1" }))
                }
                Some("storage.read") => Ok(self.kv.lock().get(&k).cloned().unwrap_or(Value::Null)),
                other => Err(Error::InvalidArgument(format!("unknown op {other:?}"))),
            }
        }
    }

    #[tokio::test]
    async fn component_guest_uses_typed_storage_imports() {
        let rt = Arc::new(Runtime::new(Limits::default()).unwrap());
        let host = Arc::new(MemHost::default());
        rt.set_host(host.clone());
        let ids = rt.load_bytes("kv", TYPED_COMPONENT.as_bytes()).unwrap();
        assert_eq!(ids, vec!["kv".to_string()]);
        let out = rt
            .call("kv", CallContext::default(), b"hello".to_vec())
            .await
            .unwrap();
        assert_eq!(
            out, b"\"hello\"",
            "read returns the JSON of what write stored"
        );
        let calls = host.calls.lock();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            json!({ "op": "storage.write", "collection": "c", "key": "k", "value": "hello", "owner_id": null })
        );
        assert_eq!(
            calls[1],
            json!({ "op": "storage.read", "collection": "c", "key": "k", "owner_id": null })
        );
    }
}
