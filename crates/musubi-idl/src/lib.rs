//! Musubi IDL (plan 3.8): one schema file describes the game's types, enums, storage
//! collections and module RPCs; everything else is derived from it — JSON Schema contracts
//! for storage collections, request/response validation for `/v1/rpc/{name}`, OpenAPI
//! components and paths, Rust types for modules and tools.
//!
//! ```toml
//! package = "mygame"
//!
//! [enums.Class]
//! values = ["mage", "rogue"]
//!
//! [types.Vec2]
//! x = "float"
//! y = "float"
//!
//! [types.Profile]
//! name = { type = "string", min_len = 1, max_len = 24 }
//! level = { type = "int", min = 1, max = 60, doc = "Progression level" }
//! class = "Class"
//! home = "Vec2?"
//! tags = "list<string>"
//! stats = "map<int>"
//! extra = "json?"
//!
//! [collections.profiles]
//! type = "Profile"
//! version = 2
//! indexes = [{ name = "by_level", path = "/level" }]
//! migrations = [{ from = 1, set = { tags = [] } }]
//!
//! [rpcs.set_name]
//! input = "SetNameRequest"
//! output = "Profile"
//! auth = "player"
//! ```
//!
//! Type expressions: `string`, `int`, `float`, `bool`, `uuid`, `timestamp`, `json`,
//! `list<T>`, `map<T>` (string keys), a type or enum name; a trailing `?` makes a field
//! optional (absent or `null`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct IdlError(pub String);

pub type Result<T> = std::result::Result<T, IdlError>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(IdlError(m.into()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Ty {
    String,
    Int,
    Float,
    Bool,
    Uuid,
    Timestamp,
    Json,
    List(Box<Ty>),
    Map(Box<Ty>),
    Named(String),
}

impl Ty {
    /// Parse a type expression without the optional marker.
    pub fn parse(s: &str) -> Result<Ty> {
        let s = s.trim();
        Ok(match s {
            "string" => Ty::String,
            "int" | "integer" | "i64" | "i32" => Ty::Int,
            "float" | "number" | "f64" | "f32" => Ty::Float,
            "bool" | "boolean" => Ty::Bool,
            "uuid" => Ty::Uuid,
            "timestamp" | "datetime" => Ty::Timestamp,
            "json" | "any" => Ty::Json,
            _ => {
                if let Some(inner) = s.strip_prefix("list<").and_then(|r| r.strip_suffix('>')) {
                    Ty::List(Box::new(Ty::parse(inner)?))
                } else if let Some(inner) = s.strip_prefix("map<").and_then(|r| r.strip_suffix('>'))
                {
                    Ty::Map(Box::new(Ty::parse(inner)?))
                } else if is_ident(s) {
                    Ty::Named(s.to_string())
                } else {
                    return err(format!("bad type expression `{s}`"));
                }
            }
        })
    }

    fn named(&self, out: &mut BTreeSet<String>) {
        match self {
            Ty::List(t) | Ty::Map(t) => t.named(out),
            Ty::Named(n) => {
                out.insert(n.clone());
            }
            _ => {}
        }
    }
}

fn is_ident(s: &str) -> bool {
    let mut ch = s.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub ty: Ty,
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TypeDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub fields: BTreeMap<String, Field>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EnumDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionDef {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default = "one")]
    pub version: i32,
    #[serde(default)]
    pub indexes: Vec<Value>,
    #[serde(default)]
    pub migrations: Vec<Value>,
    /// Access policy expressions for user callers (`read` / `write` / `delete`), see the
    /// storage policy language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

fn one() -> i32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// A player's bearer token or a server key.
    #[default]
    Any,
    /// Bearer token only.
    Player,
    /// Server key only.
    Server,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RpcDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// `[matches.<module>]`: typed creation params and JSON state of an authoritative match
/// module (`match_<module>_*` exports).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Broadcast the JSON state to players as RFC 6902 deltas after every handler call
    /// (`match_state` snapshot on join, `match_state_delta` after). Default: on when `state`
    /// is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<bool>,
    /// Server-private JSON pointers stripped before syncing (`/deck`, `/players/*/hand`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_exclude: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// `[configs.<name>]`: a typed config document the server writes (`PUT /v1/server/configs/{name}`)
/// and clients read (`GET /v1/configs/{name}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDef {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default = "one")]
    pub version: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// A parsed, validated schema.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Idl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default)]
    pub types: BTreeMap<String, TypeDef>,
    #[serde(default)]
    pub enums: BTreeMap<String, EnumDef>,
    #[serde(default)]
    pub collections: BTreeMap<String, CollectionDef>,
    #[serde(default)]
    pub rpcs: BTreeMap<String, RpcDef>,
    #[serde(default)]
    pub matches: BTreeMap<String, MatchDef>,
    #[serde(default)]
    pub configs: BTreeMap<String, ConfigDef>,
    /// Module HTTP handlers: key `module/name` → export `http_<name>` of module `module`,
    /// mounted at `/v1/x/{module}/{name}`.
    #[serde(default)]
    pub http: BTreeMap<String, HttpDef>,
    /// Module environment (P1-63): key `module/KEY` → typed setting the module reads through the
    /// `env` host interface; values are bound per deployment (`/v1/admin/runtime/modules/{m}/env`).
    #[serde(default)]
    pub env: BTreeMap<String, EnvDef>,
}

/// `[env."module/KEY"] type = "string" | "int" | "float" | "bool" | "json", required, default,
/// secret, doc`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EnvDef {
    #[serde(rename = "type")]
    pub ty: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Sealed at rest, redacted in the admin API, never written to logs/events.
    pub secret: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// Who may call a module HTTP handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpAuth {
    /// Bearer player token or server key (the default, like RPCs).
    #[default]
    Any,
    Player,
    Server,
    /// No credentials required (public webhook-style endpoint).
    None,
    /// HTTP Basic auth against `basic.users`.
    Basic,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpBasic {
    pub realm: Option<String>,
    /// user → password.
    pub users: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpCors {
    /// Allowed origins; `*` allows any. Empty disables CORS handling.
    pub origins: Vec<String>,
    /// Allowed methods for preflight (defaults to the handler's `methods`).
    pub methods: Vec<String>,
    /// Allowed request headers for preflight.
    pub headers: Vec<String>,
    pub expose_headers: Vec<String>,
    pub credentials: bool,
    pub max_age_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpDef {
    /// Allowed HTTP methods (upper-case); empty = any.
    pub methods: Vec<String>,
    pub auth: HttpAuth,
    pub basic: Option<HttpBasic>,
    pub cors: Option<HttpCors>,
    /// Per-route body limit (bytes); `None` = runtime default.
    pub max_body_bytes: Option<usize>,
    /// Per-route calls per second per caller (ip or account); `None` = unlimited.
    pub rate_per_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

// ---- TOML surface -------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum FieldSpec {
    Short(String),
    Long(FieldTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldTable {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    optional: Option<bool>,
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    min_len: Option<u64>,
    #[serde(default)]
    max_len: Option<u64>,
    #[serde(default)]
    default: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TypeSpec {
    /// `[types.X]` with fields directly under it (`doc` reserved for the type's description).
    Fields(BTreeMap<String, FieldSpec>),
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Doc {
    package: Option<String>,
    types: BTreeMap<String, TypeSpec>,
    enums: BTreeMap<String, EnumDef>,
    collections: BTreeMap<String, CollectionDef>,
    rpcs: BTreeMap<String, RpcDef>,
    matches: BTreeMap<String, MatchDef>,
    configs: BTreeMap<String, ConfigDef>,
    http: BTreeMap<String, HttpDef>,
    env: BTreeMap<String, EnvDef>,
}

fn toml_to_json(v: toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl Idl {
    pub fn parse(text: &str) -> Result<Idl> {
        let doc: Doc = toml::from_str(text).map_err(|e| IdlError(format!("idl: {e}")))?;
        let mut idl = Idl {
            package: doc.package,
            enums: doc.enums,
            collections: doc.collections,
            rpcs: doc.rpcs,
            matches: doc.matches,
            configs: doc.configs,
            http: doc.http,
            env: doc.env,
            ..Default::default()
        };
        for (name, spec) in doc.types {
            let TypeSpec::Fields(fields) = spec;
            let mut def = TypeDef::default();
            for (fname, fs) in fields {
                if fname == "doc" {
                    if let FieldSpec::Short(d) = fs {
                        def.doc = Some(d);
                        continue;
                    }
                }
                let field = match fs {
                    FieldSpec::Short(expr) => {
                        let (expr, optional) = split_optional(&expr);
                        Field {
                            ty: Ty::parse(expr).map_err(|e| at(&name, &fname, e))?,
                            optional,
                            doc: None,
                            min: None,
                            max: None,
                            min_len: None,
                            max_len: None,
                            default: None,
                        }
                    }
                    FieldSpec::Long(t) => {
                        let (expr, opt) = split_optional(&t.ty);
                        Field {
                            ty: Ty::parse(expr).map_err(|e| at(&name, &fname, e))?,
                            optional: opt || t.optional.unwrap_or(false),
                            doc: t.doc,
                            min: t.min,
                            max: t.max,
                            min_len: t.min_len,
                            max_len: t.max_len,
                            default: t.default.map(toml_to_json),
                        }
                    }
                };
                def.fields.insert(fname, field);
            }
            idl.types.insert(name, def);
        }
        idl.validate()?;
        Ok(idl)
    }

    /// Every reference resolves, names are identifiers, enums are non-empty, constraints fit
    /// their types.
    pub fn validate(&self) -> Result<()> {
        const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
        for (key, h) in &self.http {
            let Some((module, name)) = key.split_once('/') else {
                return err(format!("http `{key}`: key must be `module/name`"));
            };
            if module.is_empty() || !is_ident(name) {
                return err(format!("http `{key}`: bad module or handler name"));
            }
            for m in &h.methods {
                if !METHODS.contains(&m.as_str()) {
                    return err(format!("http `{key}`: unknown method `{m}`"));
                }
            }
            if h.auth == HttpAuth::Basic && h.basic.as_ref().is_none_or(|b| b.users.is_empty()) {
                return err(format!(
                    "http `{key}`: auth = \"basic\" needs `basic.users`"
                ));
            }
            if let Some(c) = &h.cors {
                for m in &c.methods {
                    if !METHODS.contains(&m.as_str()) {
                        return err(format!("http `{key}`: unknown cors method `{m}`"));
                    }
                }
            }
            if h.rate_per_sec.is_some_and(|r| r <= 0.0) {
                return err(format!("http `{key}`: rate_per_sec must be positive"));
            }
        }
        for name in self.types.keys().chain(self.enums.keys()) {
            if !is_ident(name) {
                return err(format!("`{name}` is not a valid type name"));
            }
            if self.types.contains_key(name) && self.enums.contains_key(name) {
                return err(format!("`{name}` is both a type and an enum"));
            }
        }
        for (name, e) in &self.enums {
            if e.values.is_empty() {
                return err(format!("enum `{name}` has no values"));
            }
            let set: BTreeSet<&String> = e.values.iter().collect();
            if set.len() != e.values.len() {
                return err(format!("enum `{name}` repeats a value"));
            }
        }
        for (tname, t) in &self.types {
            for (fname, f) in &t.fields {
                if !is_ident(fname) {
                    return err(format!("{tname}.{fname}: not a valid field name"));
                }
                let mut refs = BTreeSet::new();
                f.ty.named(&mut refs);
                for r in refs {
                    if !self.types.contains_key(&r) && !self.enums.contains_key(&r) {
                        return err(format!("{tname}.{fname}: unknown type `{r}`"));
                    }
                }
                let numeric = matches!(f.ty, Ty::Int | Ty::Float);
                if (f.min.is_some() || f.max.is_some()) && !numeric {
                    return err(format!(
                        "{tname}.{fname}: min/max apply to int and float only"
                    ));
                }
                let sized = matches!(f.ty, Ty::String | Ty::List(_) | Ty::Map(_));
                if (f.min_len.is_some() || f.max_len.is_some()) && !sized {
                    return err(format!(
                        "{tname}.{fname}: min_len/max_len apply to string, list and map only"
                    ));
                }
            }
        }
        for (cname, c) in &self.collections {
            if !self.types.contains_key(&c.ty) {
                return err(format!("collection `{cname}`: unknown type `{}`", c.ty));
            }
            if c.version < 1 {
                return err(format!("collection `{cname}`: version must be >= 1"));
            }
        }
        for (rname, r) in &self.rpcs {
            if !is_ident(rname) {
                return err(format!("rpc `{rname}`: not a valid name"));
            }
            for (what, t) in [("input", &r.input), ("output", &r.output)] {
                if let Some(t) = t {
                    if !self.types.contains_key(t) {
                        return err(format!("rpc `{rname}`: unknown {what} type `{t}`"));
                    }
                }
            }
        }
        for (mname, m) in &self.matches {
            if !is_ident(mname) {
                return err(format!("match `{mname}`: not a valid module name"));
            }
            for (what, t) in [("params", &m.params), ("state", &m.state)] {
                if let Some(t) = t {
                    if !self.types.contains_key(t) {
                        return err(format!("match `{mname}`: unknown {what} type `{t}`"));
                    }
                }
            }
        }
        for (cname, c) in &self.configs {
            if !is_ident(cname) {
                return err(format!("config `{cname}`: not a valid name"));
            }
            if !self.types.contains_key(&c.ty) {
                return err(format!("config `{cname}`: unknown type `{}`", c.ty));
            }
            if c.version < 1 {
                return err(format!("config `{cname}`: version must be >= 1"));
            }
        }
        Ok(())
    }

    // ---- JSON Schema ------------------------------------------------------------------------

    /// Types and enums reachable from `root` (excluding it).
    fn reachable(&self, root: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![root.to_string()];
        while let Some(n) = stack.pop() {
            if let Some(t) = self.types.get(&n) {
                for f in t.fields.values() {
                    let mut refs = BTreeSet::new();
                    f.ty.named(&mut refs);
                    for r in refs {
                        if seen.insert(r.clone()) {
                            stack.push(r);
                        }
                    }
                }
            }
        }
        seen.remove(root);
        seen
    }

    fn ty_schema(&self, ty: &Ty, refs: &str, formats: bool) -> Value {
        match ty {
            Ty::String => json!({ "type": "string" }),
            Ty::Int => json!({ "type": "integer" }),
            Ty::Float => json!({ "type": "number" }),
            Ty::Bool => json!({ "type": "boolean" }),
            Ty::Uuid if formats => json!({ "type": "string", "format": "uuid" }),
            Ty::Timestamp if formats => json!({ "type": "string", "format": "date-time" }),
            Ty::Uuid | Ty::Timestamp => json!({ "type": "string" }),
            Ty::Json => json!({}),
            Ty::List(t) => json!({ "type": "array", "items": self.ty_schema(t, refs, formats) }),
            Ty::Map(t) => json!({
                "type": "object",
                "additionalProperties": self.ty_schema(t, refs, formats)
            }),
            Ty::Named(n) => {
                if let Some(e) = self.enums.get(n) {
                    json!({ "enum": e.values })
                } else {
                    json!({ "$ref": format!("{refs}{n}") })
                }
            }
        }
    }

    fn field_schema(&self, f: &Field, refs: &str, formats: bool) -> Value {
        let mut s = self.ty_schema(&f.ty, refs, formats);
        if let Some(o) = s.as_object_mut() {
            if let Some(m) = f.min {
                o.insert("minimum".into(), json!(m));
            }
            if let Some(m) = f.max {
                o.insert("maximum".into(), json!(m));
            }
            match f.ty {
                Ty::String => {
                    if let Some(m) = f.min_len {
                        o.insert("minLength".into(), json!(m));
                    }
                    if let Some(m) = f.max_len {
                        o.insert("maxLength".into(), json!(m));
                    }
                }
                Ty::List(_) => {
                    if let Some(m) = f.min_len {
                        o.insert("minItems".into(), json!(m));
                    }
                    if let Some(m) = f.max_len {
                        o.insert("maxItems".into(), json!(m));
                    }
                }
                Ty::Map(_) => {
                    if let Some(m) = f.min_len {
                        o.insert("minProperties".into(), json!(m));
                    }
                    if let Some(m) = f.max_len {
                        o.insert("maxProperties".into(), json!(m));
                    }
                }
                _ => {}
            }
            if formats {
                if let Some(d) = &f.doc {
                    o.insert("description".into(), json!(d));
                }
                if let Some(d) = &f.default {
                    o.insert("default".into(), d.clone());
                }
            }
        }
        if f.optional {
            json!({ "anyOf": [s, { "type": "null" }] })
        } else {
            s
        }
    }

    /// Object schema of one type (no `$defs`), refs pointing at `refs`.
    fn object_schema(&self, name: &str, refs: &str, formats: bool) -> Value {
        let t = &self.types[name];
        let mut props = Map::new();
        let mut required = Vec::new();
        for (fname, f) in &t.fields {
            props.insert(fname.clone(), self.field_schema(f, refs, formats));
            if !f.optional {
                required.push(fname.clone());
            }
        }
        let mut s = json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false
        });
        if formats {
            if let Some(d) = &t.doc {
                s["description"] = json!(d);
            }
        }
        s
    }

    /// Self-contained JSON Schema (draft 2020-12 subset the storage validator understands) for
    /// type `name`: the object inline, referenced types under `$defs`.
    pub fn json_schema(&self, name: &str) -> Result<Value> {
        if !self.types.contains_key(name) {
            return err(format!("unknown type `{name}`"));
        }
        let mut s = self.object_schema(name, "#/$defs/", false);
        let defs: Map<String, Value> = self
            .reachable(name)
            .into_iter()
            .filter(|n| self.types.contains_key(n))
            .map(|n| {
                let d = self.object_schema(&n, "#/$defs/", false);
                (n, d)
            })
            .collect();
        if !defs.is_empty() {
            s["$defs"] = Value::Object(defs);
        }
        Ok(s)
    }

    /// Storage collection contracts (`CollectionSchema` JSON: collection, version, schema,
    /// migrations, indexes) for every `[collections.*]`.
    pub fn collection_schemas(&self) -> Result<Vec<Value>> {
        self.collections
            .iter()
            .map(|(cname, c)| {
                Ok(json!({
                    "collection": cname,
                    "version": c.version,
                    "schema": self.json_schema(&c.ty)?,
                    "migrations": c.migrations,
                    "indexes": c.indexes,
                    "policy": c.policy,
                }))
            })
            .collect()
    }

    // ---- OpenAPI ----------------------------------------------------------------------------

    /// `components.schemas` entries for every type (refs into `#/components/schemas/`).
    pub fn openapi_components(&self) -> Map<String, Value> {
        let prefix = self
            .package
            .as_deref()
            .map(|p| format!("{p}."))
            .unwrap_or_default();
        self.types
            .keys()
            .map(|n| {
                let mut s = self.object_schema(n, "#/components/schemas/", true);
                if !prefix.is_empty() {
                    rewrite_refs(&mut s, &prefix);
                }
                (format!("{prefix}{n}"), s)
            })
            .collect()
    }

    /// `paths` entries for every rpc: `POST /v1/rpc/{name}`.
    pub fn openapi_paths(&self) -> Map<String, Value> {
        let prefix = self
            .package
            .as_deref()
            .map(|p| format!("{p}."))
            .unwrap_or_default();
        let r = |t: &str| json!({ "$ref": format!("#/components/schemas/{prefix}{t}") });
        let mut paths: Map<String, Value> = self
            .configs
            .iter()
            .map(|(name, c)| {
                (
                    format!("/v1/configs/{name}"),
                    json!({ "get": {
                        "operationId": format!("config_{name}"),
                        "tags": ["configs"],
                        "summary": c.doc.clone().unwrap_or_else(|| format!("Config `{name}`")),
                        "security": [{ "bearer": [] }],
                        "responses": {
                            "200": { "description": "OK", "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": { "name": { "type": "string" }, "version": { "type": "integer" },
                                                "updated_at": { "type": "integer" }, "value": r(&c.ty) }
                            } } } },
                            "default": { "$ref": "#/components/responses/Error" }
                        }
                    } }),
                )
            })
            .collect();
        paths.extend(self.rpcs.iter().map(|(name, rpc)| {
            let security = match rpc.auth {
                Auth::Any => json!([{ "bearer": [] }, { "serverKey": [] }]),
                Auth::Player => json!([{ "bearer": [] }]),
                Auth::Server => json!([{ "serverKey": [] }]),
            };
            let mut op = json!({
                "operationId": format!("rpc_{name}"),
                "tags": ["rpc"],
                "summary": rpc.doc.clone().unwrap_or_else(|| format!("RPC `{name}`")),
                "security": security,
                "responses": {
                    "200": { "description": "OK" },
                    "default": { "$ref": "#/components/responses/Error" }
                }
            });
            if let Some(i) = &rpc.input {
                op["requestBody"] = json!({
                    "required": true,
                    "content": { "application/json": { "schema": r(i) } }
                });
            }
            if let Some(o) = &rpc.output {
                op["responses"]["200"]["content"] =
                    json!({ "application/json": { "schema": r(o) } });
            }
            (format!("/v1/rpc/{name}"), json!({ "post": op }))
        }));
        paths
    }

    // ---- Rust -------------------------------------------------------------------------------

    /// Rust source: serde structs and enums for every type and enum.
    pub fn rust(&self) -> String {
        let mut out = String::from(
            "// Generated by musubi-idl — do not edit.\n#![allow(dead_code, clippy::all)]\nuse serde::{Deserialize, Serialize};\n\n",
        );
        for (name, e) in &self.enums {
            if let Some(d) = &e.doc {
                out.push_str(&format!("/// {d}\n"));
            }
            out.push_str(
                "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]\n",
            );
            out.push_str(&format!("pub enum {name} {{\n"));
            for v in &e.values {
                out.push_str(&format!(
                    "    #[serde(rename = \"{v}\")]\n    {},\n",
                    pascal(v)
                ));
            }
            out.push_str("}\n\n");
        }
        for (name, t) in &self.types {
            if let Some(d) = &t.doc {
                out.push_str(&format!("/// {d}\n"));
            }
            out.push_str("#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]\n");
            out.push_str(&format!("pub struct {name} {{\n"));
            for (fname, f) in &t.fields {
                if let Some(d) = &f.doc {
                    out.push_str(&format!("    /// {d}\n"));
                }
                let ty = rust_ty(&f.ty);
                if f.optional {
                    out.push_str(
                        "    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n",
                    );
                    out.push_str(&format!("    pub {}: Option<{ty}>,\n", rust_field(fname)));
                } else {
                    out.push_str(&format!("    pub {}: {ty},\n", rust_field(fname)));
                }
            }
            out.push_str("}\n\n");
        }
        out
    }

    /// Rust source for a WASM guest module (ABI v0 of `musubi-runtime`): the types of `rust()`
    /// plus `CallContext`/`Presence`/`Input`/`Outbound`, the `alloc`/`musubi.*` glue, a `Rpcs`
    /// trait with one typed method per `[rpcs.*]` and a `<Module>Match` trait per `[matches.*]`,
    /// and the `rpc_<name>` / `match_<module>_*` exports that decode JSON, call
    /// `crate::Handlers` and encode the reply. Include it as `mod bindings;` and
    /// `impl bindings::Rpcs for Handlers` (`pub struct Handlers;` at the crate root).
    pub fn guest_rust(&self) -> String {
        let mut out = self.rust();
        out.push_str(GUEST_PRELUDE);
        // rpcs
        out.push_str("/// One method per `[rpcs.*]`; implement it for `crate::Handlers`.\npub trait Rpcs {\n");
        for (name, r) in &self.rpcs {
            if let Some(d) = &r.doc {
                out.push_str(&format!("    /// {d}\n"));
            }
            out.push_str(&format!(
                "    fn {}(ctx: &CallContext, input: {}) -> Result<{}, String>;\n",
                rust_field(name),
                r.input
                    .clone()
                    .unwrap_or_else(|| "serde_json::Value".into()),
                r.output
                    .clone()
                    .unwrap_or_else(|| "serde_json::Value".into()),
            ));
        }
        out.push_str("}\n\n");
        for (name, r) in &self.rpcs {
            let input = r
                .input
                .clone()
                .unwrap_or_else(|| "serde_json::Value".into());
            out.push_str(&format!(
                "#[no_mangle]\npub extern \"C\" fn rpc_{name}(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::rpc(c, cl, p, pl, |ctx, input: {input}| <crate::Handlers as Rpcs>::{}(ctx, input))\n}}\n\n",
                rust_field(name)
            ));
        }
        // matches
        for (module, m) in &self.matches {
            let tr = format!("{}Match", pascal(module));
            let st = m
                .state
                .clone()
                .unwrap_or_else(|| "serde_json::Value".into());
            let pa = m
                .params
                .clone()
                .unwrap_or_else(|| "serde_json::Value".into());
            if let Some(d) = &m.doc {
                out.push_str(&format!("/// {d}\n"));
            }
            out.push_str(&format!(
                "/// Authoritative match module `{module}` (`match_{module}_*` exports); implement for `crate::Handlers`.\n\
pub trait {tr} {{\n\
    fn init(params: {pa}) -> Result<MatchInit<{st}>, String>;\n\
    fn join_attempt(state: {st}, presence: &Presence, metadata: &std::collections::BTreeMap<String, String>, members: &[Presence]) -> Result<({st}, Result<(), String>), String>;\n\
    fn join(state: {st}, joined: &[Presence], members: &[Presence]) -> Result<{st}, String>;\n\
    fn leave(state: {st}, left: &[Presence], members: &[Presence]) -> Result<{st}, String>;\n\
    fn tick(state: {st}, tick: u64, inputs: Vec<Input>, members: &[Presence]) -> Result<MatchTick<{st}>, String>;\n\
    fn terminate(state: {st}, grace_seconds: u32) -> Result<{st}, String>;\n\
}}\n\n"
            ));
            out.push_str(&format!(
                "#[no_mangle]\npub extern \"C\" fn match_{module}_init(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::match_init::<{pa}, {st}, _>(c, cl, p, pl, <crate::Handlers as {tr}>::init)\n}}\n\
#[no_mangle]\npub extern \"C\" fn match_{module}_join_attempt(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::match_join_attempt::<{st}, _>(c, cl, p, pl, <crate::Handlers as {tr}>::join_attempt)\n}}\n\
#[no_mangle]\npub extern \"C\" fn match_{module}_join(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::match_presences::<{st}, _>(c, cl, p, pl, <crate::Handlers as {tr}>::join)\n}}\n\
#[no_mangle]\npub extern \"C\" fn match_{module}_leave(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::match_presences::<{st}, _>(c, cl, p, pl, <crate::Handlers as {tr}>::leave)\n}}\n\
#[no_mangle]\npub extern \"C\" fn match_{module}_tick(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::match_tick::<{st}, _>(c, cl, p, pl, <crate::Handlers as {tr}>::tick)\n}}\n\
#[no_mangle]\npub extern \"C\" fn match_{module}_terminate(c: i32, cl: i32, p: i32, pl: i32) -> i64 {{\n    abi::match_terminate::<{st}, _>(c, cl, p, pl, <crate::Handlers as {tr}>::terminate)\n}}\n\n"
            ));
        }
        out
    }

    /// Markdown reference of the document: types, enums, collections, RPCs, matches, configs.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# {}\n\nGenerated by musubi-idl — do not edit.\n\n",
            self.package.as_deref().unwrap_or("API")
        ));
        if !self.enums.is_empty() {
            out.push_str("## Enums\n\n");
            for (name, e) in &self.enums {
                out.push_str(&format!("### `{name}`\n\n"));
                if let Some(d) = &e.doc {
                    out.push_str(&format!("{d}\n\n"));
                }
                out.push_str(&format!(
                    "Values: {}\n\n",
                    e.values
                        .iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        if !self.types.is_empty() {
            out.push_str("## Types\n\n");
            for (name, t) in &self.types {
                out.push_str(&format!("### `{name}`\n\n"));
                if let Some(d) = &t.doc {
                    out.push_str(&format!("{d}\n\n"));
                }
                out.push_str("| Field | Type | Constraints | Description |\n|---|---|---|---|\n");
                for (fname, f) in &t.fields {
                    let mut cons = Vec::new();
                    if let Some(v) = f.min {
                        cons.push(format!("min {v}"));
                    }
                    if let Some(v) = f.max {
                        cons.push(format!("max {v}"));
                    }
                    if let Some(v) = f.min_len {
                        cons.push(format!("min_len {v}"));
                    }
                    if let Some(v) = f.max_len {
                        cons.push(format!("max_len {v}"));
                    }
                    if let Some(v) = &f.default {
                        cons.push(format!("default `{v}`"));
                    }
                    out.push_str(&format!(
                        "| `{fname}` | `{}{}` | {} | {} |\n",
                        ty_str(&f.ty),
                        if f.optional { "?" } else { "" },
                        cons.join(", "),
                        f.doc.clone().unwrap_or_default()
                    ));
                }
                out.push('\n');
            }
        }
        if !self.collections.is_empty() {
            out.push_str("## Storage collections\n\n| Collection | Type | Version | Indexes | Description |\n|---|---|---|---|---|\n");
            for (name, c) in &self.collections {
                out.push_str(&format!(
                    "| `{name}` | `{}` | {} | {} | {} |\n",
                    c.ty,
                    c.version,
                    c.indexes.len(),
                    c.doc.clone().unwrap_or_default()
                ));
            }
            out.push('\n');
        }
        if !self.rpcs.is_empty() {
            out.push_str("## RPCs (`POST /v1/rpc/{name}`)\n\n| RPC | Input | Output | Auth | Description |\n|---|---|---|---|---|\n");
            for (name, r) in &self.rpcs {
                out.push_str(&format!(
                    "| `{name}` | {} | {} | {} | {} |\n",
                    r.input
                        .as_deref()
                        .map(|t| format!("`{t}`"))
                        .unwrap_or_else(|| "—".into()),
                    r.output
                        .as_deref()
                        .map(|t| format!("`{t}`"))
                        .unwrap_or_else(|| "—".into()),
                    match r.auth {
                        Auth::Any => "any",
                        Auth::Player => "player",
                        Auth::Server => "server",
                    },
                    r.doc.clone().unwrap_or_default()
                ));
            }
            out.push('\n');
        }
        if !self.matches.is_empty() {
            out.push_str("## Match modules\n\n| Module | Params | State | Description |\n|---|---|---|---|\n");
            for (name, m) in &self.matches {
                out.push_str(&format!(
                    "| `{name}` | {} | {} | {} |\n",
                    m.params
                        .as_deref()
                        .map(|t| format!("`{t}`"))
                        .unwrap_or_else(|| "—".into()),
                    m.state
                        .as_deref()
                        .map(|t| format!("`{t}`"))
                        .unwrap_or_else(|| "—".into()),
                    m.doc.clone().unwrap_or_default()
                ));
            }
            out.push('\n');
        }
        if !self.configs.is_empty() {
            out.push_str("## Configs (`GET /v1/configs/{name}`)\n\n| Config | Type | Version | Description |\n|---|---|---|---|\n");
            for (name, c) in &self.configs {
                out.push_str(&format!(
                    "| `{name}` | `{}` | {} | {} |\n",
                    c.ty,
                    c.version,
                    c.doc.clone().unwrap_or_default()
                ));
            }
            out.push('\n');
        }
        out
    }

    /// Validate an RPC payload against the declared `input` type with the storage validator
    /// semantics — exposed as the schema so the server can run its own validator.
    pub fn rpc_input_schema(&self, rpc: &str) -> Option<Value> {
        let r = self.rpcs.get(rpc)?;
        let t = r.input.as_deref()?;
        self.json_schema(t).ok()
    }

    pub fn rpc_output_schema(&self, rpc: &str) -> Option<Value> {
        let r = self.rpcs.get(rpc)?;
        let t = r.output.as_deref()?;
        self.json_schema(t).ok()
    }

    /// `[matches.<module>] params` as JSON Schema.
    pub fn match_params_schema(&self, module: &str) -> Option<Value> {
        let t = self.matches.get(module)?.params.as_deref()?;
        self.json_schema(t).ok()
    }

    /// `[matches.<module>] state` as JSON Schema.
    pub fn match_state_schema(&self, module: &str) -> Option<Value> {
        let t = self.matches.get(module)?.state.as_deref()?;
        self.json_schema(t).ok()
    }

    /// `Some(private paths)` when the module's JSON state is synced to players.
    pub fn match_sync(&self, module: &str) -> Option<Vec<String>> {
        let m = self.matches.get(module)?;
        if m.state.is_none() || m.sync == Some(false) {
            return None;
        }
        Some(m.sync_exclude.clone())
    }

    /// `[configs.<name>] type` as JSON Schema.
    pub fn config_schema(&self, name: &str) -> Option<Value> {
        let c = self.configs.get(name)?;
        self.json_schema(&c.ty).ok()
    }
}

fn rewrite_refs(v: &mut Value, prefix: &str) {
    match v {
        Value::Object(o) => {
            if let Some(Value::String(r)) = o.get_mut("$ref") {
                if let Some(rest) = r.strip_prefix("#/components/schemas/") {
                    *r = format!("#/components/schemas/{prefix}{rest}");
                }
            }
            for x in o.values_mut() {
                rewrite_refs(x, prefix);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rewrite_refs(x, prefix)),
        _ => {}
    }
}

fn split_optional(expr: &str) -> (&str, bool) {
    match expr.trim().strip_suffix('?') {
        Some(e) => (e, true),
        None => (expr.trim(), false),
    }
}

fn at(ty: &str, field: &str, e: IdlError) -> IdlError {
    IdlError(format!("{ty}.{field}: {}", e.0))
}

fn rust_ty(t: &Ty) -> String {
    match t {
        Ty::String | Ty::Timestamp => "String".into(),
        Ty::Int => "i64".into(),
        Ty::Float => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::Uuid => "uuid::Uuid".into(),
        Ty::Json => "serde_json::Value".into(),
        Ty::List(i) => format!("Vec<{}>", rust_ty(i)),
        Ty::Map(i) => format!("std::collections::BTreeMap<String, {}>", rust_ty(i)),
        Ty::Named(n) => n.clone(),
    }
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn",
];

fn rust_field(name: &str) -> String {
    if RUST_KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_string()
    }
}

fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    out
}

fn ty_str(t: &Ty) -> String {
    match t {
        Ty::String => "string".into(),
        Ty::Int => "int".into(),
        Ty::Float => "float".into(),
        Ty::Bool => "bool".into(),
        Ty::Uuid => "uuid".into(),
        Ty::Timestamp => "timestamp".into(),
        Ty::Json => "json".into(),
        Ty::List(i) => format!("list<{}>", ty_str(i)),
        Ty::Map(i) => format!("map<{}>", ty_str(i)),
        Ty::Named(n) => n.clone(),
    }
}

/// Guest-side glue appended to `guest_rust()`: wire types and the ABI v0 helpers.
const GUEST_PRELUDE: &str = r##"
// ---- Musubi guest ABI v0 ------------------------------------------------------------------

/// Who is calling (`CallContext` of the node, as JSON).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallContext {
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub server: bool,
    #[serde(default)]
    pub vars: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Presence {
    pub account_id: String,
    pub session_id: String,
    pub username: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub rich: Option<serde_json::Value>,
    #[serde(default)]
    pub invisible: bool,
}

/// A client input delivered to `tick`; `data` is the raw bytes the client sent.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "abi::InputWire")]
pub struct Input {
    pub presence: Presence,
    pub op_code: i64,
    pub data: Vec<u8>,
    pub reliable: bool,
}

/// A message to send from `tick`; empty `to` = everyone in the match.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(into = "abi::OutboundWire")]
pub struct Outbound {
    pub to: Vec<String>,
    pub op_code: i64,
    pub data: Vec<u8>,
    pub reliable: bool,
}

#[derive(Debug, Clone)]
pub struct MatchInit<S> {
    pub state: S,
    pub tick_rate: u32,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct MatchTick<S> {
    pub state: S,
    pub outbound: Vec<Outbound>,
    pub label: Option<String>,
    pub terminate: bool,
}

pub mod abi {
    use super::*;
    use serde::de::DeserializeOwned;

    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "musubi")]
    extern "C" {
        fn log(level: i32, ptr: i32, len: i32);
        fn error(ptr: i32, len: i32);
        fn host_call(ptr: i32, len: i32) -> i64;
    }
    #[cfg(not(target_arch = "wasm32"))]
    unsafe fn log(_level: i32, _ptr: i32, _len: i32) {}
    #[cfg(not(target_arch = "wasm32"))]
    unsafe fn error(_ptr: i32, _len: i32) {}
    #[cfg(not(target_arch = "wasm32"))]
    unsafe fn host_call(_ptr: i32, _len: i32) -> i64 {
        0
    }

    /// The node allocates request regions through this export.
    #[no_mangle]
    pub extern "C" fn alloc(len: i32) -> i32 {
        let mut v: Vec<u8> = Vec::with_capacity(len.max(1) as usize);
        let p = v.as_mut_ptr();
        std::mem::forget(v);
        p as usize as i32
    }

    pub fn bytes(ptr: i32, len: i32) -> &'static [u8] {
        if len <= 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(ptr as usize as *const u8, len as usize) }
    }

    pub fn output(bytes: Vec<u8>) -> i64 {
        let len = bytes.len() as i64;
        let ptr = bytes.as_ptr() as usize as i64;
        std::mem::forget(bytes);
        (ptr << 32) | len
    }

    /// Fail the current call with `msg` (the node turns it into an error reply).
    pub fn fail(msg: &str) -> i64 {
        unsafe { error(msg.as_ptr() as usize as i32, msg.len() as i32) };
        0
    }

    /// Log line on the node (`level`: 0 error, 1 warn, 2 info, 3 debug).
    pub fn log_line(level: i32, msg: &str) {
        unsafe { log(level, msg.as_ptr() as usize as i32, msg.len() as i32) }
    }

    /// Call back into the node (`host_call` ops, e.g. `{"op": "storage.read", ...}`).
    pub fn host(req: &serde_json::Value) -> Result<serde_json::Value, String> {
        let b = serde_json::to_vec(req).map_err(|e| e.to_string())?;
        let r = unsafe { host_call(b.as_ptr() as usize as i32, b.len() as i32) };
        if r == 0 {
            return Err("host call failed".into());
        }
        serde_json::from_slice(bytes((r >> 32) as i32, (r & 0xffff_ffff) as i32))
            .map_err(|e| format!("host reply: {e}"))
    }

    fn json<T: DeserializeOwned>(b: &[u8], what: &str) -> Result<T, String> {
        let b = if b.is_empty() { b"{}" } else { b };
        serde_json::from_slice(b).map_err(|e| format!("bad {what}: {e}"))
    }

    fn reply<T: Serialize>(r: Result<T, String>) -> i64 {
        match r.and_then(|v| serde_json::to_vec(&v).map_err(|e| e.to_string())) {
            Ok(b) => output(b),
            Err(e) => fail(&e),
        }
    }

    pub fn rpc<I: DeserializeOwned, O: Serialize>(
        c: i32,
        cl: i32,
        p: i32,
        pl: i32,
        f: impl FnOnce(&CallContext, I) -> Result<O, String>,
    ) -> i64 {
        reply((|| {
            let ctx: CallContext = json(bytes(c, cl), "context")?;
            let input: I = json(bytes(p, pl), "input")?;
            f(&ctx, input)
        })())
    }

    // -- base64 (standard alphabet, padded), so the guest needs no extra crate
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn b64_encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let n = chunk.len();
            let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
            let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(B64[(v >> 18) as usize & 63] as char);
            out.push(B64[(v >> 12) as usize & 63] as char);
            out.push(if n > 1 { B64[(v >> 6) as usize & 63] as char } else { '=' });
            out.push(if n > 2 { B64[v as usize & 63] as char } else { '=' });
        }
        out
    }

    pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let mut buf = 0u32;
        let mut bits = 0;
        for c in s.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                b'=' | b'\n' | b'\r' | b' ' => continue,
                _ => return Err("bad base64".into()),
            };
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Ok(out)
    }

    #[derive(Deserialize)]
    pub struct InputWire {
        presence: Presence,
        op_code: i64,
        #[serde(default)]
        data: String,
        #[serde(default)]
        reliable: bool,
    }

    impl From<InputWire> for Input {
        fn from(w: InputWire) -> Self {
            Input {
                presence: w.presence,
                op_code: w.op_code,
                data: b64_decode(&w.data).unwrap_or_default(),
                reliable: w.reliable,
            }
        }
    }

    #[derive(Serialize)]
    pub struct OutboundWire {
        to: Vec<String>,
        op_code: i64,
        data: String,
        reliable: bool,
    }

    impl From<Outbound> for OutboundWire {
        fn from(o: Outbound) -> Self {
            OutboundWire {
                to: o.to,
                op_code: o.op_code,
                data: b64_encode(&o.data),
                reliable: o.reliable,
            }
        }
    }

    // -- match handlers: state crosses as base64 JSON; empty = S::default()
    fn state_in<S: DeserializeOwned + Default>(b64: &str) -> Result<S, String> {
        let b = b64_decode(b64)?;
        if b.is_empty() {
            return Ok(S::default());
        }
        serde_json::from_slice(&b).map_err(|e| format!("bad state: {e}"))
    }

    fn state_out<S: Serialize>(s: &S) -> Result<String, String> {
        serde_json::to_vec(s).map(|b| b64_encode(&b)).map_err(|e| e.to_string())
    }

    #[derive(Deserialize)]
    struct InitIn<P> {
        #[serde(default = "serde_json::Value::default")]
        params: serde_json::Value,
        #[serde(skip)]
        _p: std::marker::PhantomData<P>,
    }

    pub fn match_init<P: DeserializeOwned, S: Serialize, F>(c: i32, cl: i32, p: i32, pl: i32, f: F) -> i64
    where
        F: FnOnce(P) -> Result<MatchInit<S>, String>,
    {
        let _ = bytes(c, cl);
        reply((|| {
            let r: InitIn<P> = json(bytes(p, pl), "init payload")?;
            let params = if r.params.is_null() { serde_json::json!({}) } else { r.params };
            let params: P = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            let init = f(params)?;
            Ok(serde_json::json!({
                "state": state_out(&init.state)?,
                "tick_rate": init.tick_rate,
                "label": init.label,
            }))
        })())
    }

    #[derive(Deserialize)]
    struct JoinAttemptIn {
        #[serde(default)]
        state: String,
        presence: Presence,
        #[serde(default)]
        metadata: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        members: Vec<Presence>,
    }

    pub fn match_join_attempt<S: Serialize + DeserializeOwned + Default, F>(c: i32, cl: i32, p: i32, pl: i32, f: F) -> i64
    where
        F: FnOnce(S, &Presence, &std::collections::BTreeMap<String, String>, &[Presence]) -> Result<(S, Result<(), String>), String>,
    {
        let _ = bytes(c, cl);
        reply((|| {
            let r: JoinAttemptIn = json(bytes(p, pl), "join_attempt payload")?;
            let (state, verdict) = f(state_in(&r.state)?, &r.presence, &r.metadata, &r.members)?;
            Ok(serde_json::json!({
                "state": state_out(&state)?,
                "accept": verdict.is_ok(),
                "reason": verdict.err(),
            }))
        })())
    }

    #[derive(Deserialize)]
    struct PresencesIn {
        #[serde(default)]
        state: String,
        #[serde(default)]
        presences: Vec<Presence>,
        #[serde(default)]
        members: Vec<Presence>,
    }

    pub fn match_presences<S: Serialize + DeserializeOwned + Default, F>(c: i32, cl: i32, p: i32, pl: i32, f: F) -> i64
    where
        F: FnOnce(S, &[Presence], &[Presence]) -> Result<S, String>,
    {
        let _ = bytes(c, cl);
        reply((|| {
            let r: PresencesIn = json(bytes(p, pl), "payload")?;
            let state = f(state_in(&r.state)?, &r.presences, &r.members)?;
            Ok(serde_json::json!({ "state": state_out(&state)? }))
        })())
    }

    #[derive(Deserialize)]
    struct TickIn {
        #[serde(default)]
        state: String,
        #[serde(default)]
        tick: u64,
        #[serde(default)]
        inputs: Vec<Input>,
        #[serde(default)]
        members: Vec<Presence>,
    }

    pub fn match_tick<S: Serialize + DeserializeOwned + Default, F>(c: i32, cl: i32, p: i32, pl: i32, f: F) -> i64
    where
        F: FnOnce(S, u64, Vec<Input>, &[Presence]) -> Result<MatchTick<S>, String>,
    {
        let _ = bytes(c, cl);
        reply((|| {
            let r: TickIn = json(bytes(p, pl), "tick payload")?;
            let t = f(state_in(&r.state)?, r.tick, r.inputs, &r.members)?;
            Ok(serde_json::json!({
                "state": state_out(&t.state)?,
                "outbound": t.outbound,
                "label": t.label,
                "terminate": t.terminate,
            }))
        })())
    }

    #[derive(Deserialize)]
    struct TerminateIn {
        #[serde(default)]
        state: String,
        #[serde(default)]
        grace_seconds: u32,
    }

    pub fn match_terminate<S: Serialize + DeserializeOwned + Default, F>(c: i32, cl: i32, p: i32, pl: i32, f: F) -> i64
    where
        F: FnOnce(S, u32) -> Result<S, String>,
    {
        let _ = bytes(c, cl);
        reply((|| {
            let r: TerminateIn = json(bytes(p, pl), "terminate payload")?;
            let state = f(state_in(&r.state)?, r.grace_seconds)?;
            Ok(serde_json::json!({ "state": state_out(&state)? }))
        })())
    }
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
package = "mygame"

[enums.Class]
values = ["mage", "rogue"]

[types.Vec2]
x = "float"
y = "float"

[types.Profile]
doc = "A player's public profile"
name = { type = "string", min_len = 1, max_len = 24 }
level = { type = "int", min = 1, max = 60, doc = "Progression level" }
class = "Class"
home = "Vec2?"
tags = "list<string>"
stats = "map<int>"
extra = "json?"

[types.SetNameRequest]
name = "string"

[collections.profiles]
type = "Profile"
version = 2
indexes = [{ name = "by_level", path = "/level" }]

[rpcs.set_name]
input = "SetNameRequest"
output = "Profile"
auth = "player"
"#;

    #[test]
    fn parses_and_emits_json_schema() {
        let idl = Idl::parse(SAMPLE).unwrap();
        assert_eq!(idl.types.len(), 3);
        let p = &idl.types["Profile"];
        assert_eq!(p.doc.as_deref(), Some("A player's public profile"));
        assert!(p.fields["home"].optional && !p.fields["name"].optional);
        assert_eq!(p.fields["stats"].ty, Ty::Map(Box::new(Ty::Int)));
        let s = idl.json_schema("Profile").unwrap();
        assert_eq!(
            s["type"],
            "string"
                .parse::<Value>()
                .map(|_| json!("object"))
                .unwrap_or(json!("object"))
        );
        assert_eq!(
            s["properties"]["name"],
            json!({ "type": "string", "minLength": 1, "maxLength": 24 })
        );
        assert_eq!(s["properties"]["level"]["minimum"], json!(1.0));
        assert_eq!(
            s["properties"]["class"],
            json!({ "enum": ["mage", "rogue"] })
        );
        assert_eq!(
            s["properties"]["home"],
            json!({ "anyOf": [{ "$ref": "#/$defs/Vec2" }, { "type": "null" }] })
        );
        assert_eq!(s["$defs"]["Vec2"]["required"], json!(["x", "y"]));
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&json!("name")) && !req.contains(&json!("home")));
        assert_eq!(s["additionalProperties"], json!(false));
        // collections carry the schema, version and indexes
        let cols = idl.collection_schemas().unwrap();
        assert_eq!(cols[0]["collection"], "profiles");
        assert_eq!(cols[0]["version"], 2);
        assert_eq!(cols[0]["indexes"][0]["name"], "by_level");
        assert_eq!(cols[0]["schema"]["$defs"]["Vec2"]["type"], "object");
        // rpc schemas
        assert_eq!(
            idl.rpc_input_schema("set_name").unwrap()["required"],
            json!(["name"])
        );
        assert!(idl.rpc_input_schema("nope").is_none());
    }

    #[test]
    fn openapi_and_rust_output() {
        let idl = Idl::parse(SAMPLE).unwrap();
        let c = idl.openapi_components();
        assert!(c.contains_key("mygame.Profile"));
        assert_eq!(
            c["mygame.Profile"]["properties"]["home"]["anyOf"][0]["$ref"],
            "#/components/schemas/mygame.Vec2"
        );
        assert_eq!(
            c["mygame.Profile"]["properties"]["level"]["description"],
            "Progression level"
        );
        let p = idl.openapi_paths();
        let op = &p["/v1/rpc/set_name"]["post"];
        assert_eq!(op["security"], json!([{ "bearer": [] }]));
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/mygame.SetNameRequest"
        );
        let rs = idl.rust();
        assert!(rs.contains("pub enum Class {\n    #[serde(rename = \"mage\")]\n    Mage,"));
        assert!(rs.contains("pub home: Option<Vec2>,"));
        assert!(rs.contains("pub stats: std::collections::BTreeMap<String, i64>,"));
        assert!(rs.contains("pub extra: Option<serde_json::Value>,"));
    }

    #[test]
    fn http_handlers_parse_and_validate() {
        let idl = Idl::parse(
            r#"
[http."shop/checkout"]
methods = ["POST"]
auth = "basic"
basic = { realm = "ops", users = { admin = "pw" } }
cors = { origins = ["*"], credentials = false, max_age_secs = 60 }
max_body_bytes = 1024
rate_per_sec = 5.0
"#,
        )
        .unwrap();
        let h = &idl.http["shop/checkout"];
        assert_eq!(h.methods, vec!["POST".to_string()]);
        assert_eq!(h.auth, HttpAuth::Basic);
        assert_eq!(h.basic.as_ref().unwrap().users["admin"], "pw");
        assert_eq!(h.cors.as_ref().unwrap().max_age_secs, Some(60));
        assert_eq!(h.max_body_bytes, Some(1024));
        // defaults
        let d = Idl::parse("[http.\"m/n\"]\n").unwrap();
        assert_eq!(d.http["m/n"].auth, HttpAuth::Any);
        assert!(d.http["m/n"].methods.is_empty());
        for bad in [
            "[http.\"noslash\"]\n",
            "[http.\"m/bad name\"]\n",
            "[http.\"m/n\"]\nmethods = [\"FETCH\"]\n",
            "[http.\"m/n\"]\nauth = \"basic\"\n",
            "[http.\"m/n\"]\nauth = \"basic\"\nbasic = { users = {} }\n",
            "[http.\"m/n\"]\ncors = { origins = [\"*\"], methods = [\"NOPE\"] }\n",
            "[http.\"m/n\"]\nrate_per_sec = 0\n",
            "[http.\"m/n\"]\nauth = \"root\"\n",
        ] {
            assert!(Idl::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_schemas() {
        let bad = |s: &str| Idl::parse(s).unwrap_err().0;
        assert!(bad("[types.A]\nx = \"Nope\"").contains("unknown type `Nope`"));
        assert!(bad("[types.A]\nx = \"list<string\"").contains("bad type expression"));
        assert!(bad("[types.A]\nx = { type = \"string\", min = 1 }").contains("min/max"));
        assert!(bad("[enums.E]\nvalues = []").contains("no values"));
        assert!(bad("[types.A]\nx = \"int\"\n[collections.c]\ntype = \"B\"")
            .contains("unknown type `B`"));
        assert!(bad("[rpcs.r]\ninput = \"Z\"").contains("unknown input type"));
        assert!(bad("[types.A]\nx = { type = \"int\", bogus = 1 }").contains("idl:"));
    }

    #[test]
    fn matches_and_configs() {
        let bad = |src: &str| Idl::parse(src).err().map(|e| e.0).unwrap_or_default();
        let idl = Idl::parse(
            r#"
[types.ArenaParams]
rounds = { type = "int", min = 1, max = 9 }
[types.ArenaState]
round = "int"
scores = "map<int>"
[types.Tuning]
speed = "float"
[matches.arena]
params = "ArenaParams"
state = "ArenaState"
[configs.tuning]
type = "Tuning"
version = 2
"#,
        )
        .unwrap();
        let p = idl.match_params_schema("arena").unwrap();
        assert_eq!(p["properties"]["rounds"]["minimum"], 1.0);
        assert!(idl.match_state_schema("arena").unwrap()["properties"]["scores"].is_object());
        assert!(idl.match_params_schema("other").is_none());
        assert_eq!(idl.configs["tuning"].version, 2);
        assert_eq!(
            idl.config_schema("tuning").unwrap()["properties"]["speed"]["type"],
            "number"
        );
        let paths = idl.openapi_paths();
        assert!(paths.contains_key("/v1/configs/tuning"));
        assert!(bad("[types.A]\nx = \"int\"\n[matches.m]\nstate = \"B\"")
            .contains("unknown state type"));
        assert!(bad("[types.A]\nx = \"int\"\n[configs.c]\ntype = \"B\"").contains("unknown type"));
        assert!(
            bad("[types.A]\nx = \"int\"\n[configs.c]\ntype = \"A\"\nversion = 0")
                .contains("version")
        );
    }

    #[test]
    fn guest_bindings_and_markdown() {
        let idl = Idl::parse(
            r#"
package = "g"
[types.EchoIn]
x = "int?"
[types.ArenaState]
round = "int"
[rpcs.echo]
input = "EchoIn"
output = "EchoIn"
[rpcs.free]
[matches.arena]
state = "ArenaState"
[configs.tuning]
type = "EchoIn"
"#,
        )
        .unwrap();
        let g = idl.guest_rust();
        assert!(g.contains("pub trait Rpcs {"));
        assert!(g.contains("fn echo(ctx: &CallContext, input: EchoIn) -> Result<EchoIn, String>;"));
        assert!(g.contains("fn free(ctx: &CallContext, input: serde_json::Value) -> Result<serde_json::Value, String>;"));
        assert!(g.contains("pub extern \"C\" fn rpc_echo("));
        assert!(g.contains("pub trait ArenaMatch {"));
        assert!(g.contains("pub extern \"C\" fn match_arena_tick("));
        assert!(g.contains("abi::match_init::<serde_json::Value, ArenaState, _>"));
        assert!(g.contains("pub extern \"C\" fn alloc("));
        let md = idl.markdown();
        assert!(md.contains("| `x` | `int?` |"));
        assert!(md.contains("| `echo` | `EchoIn` | `EchoIn` | any |"));
        assert!(md.contains("| `arena` | — | `ArenaState` |"));
        assert!(md.contains("| `tuning` | `EchoIn` | 1 |"));
    }
}

/// Colyseus `Schema` classes (TypeScript, `@type(...)` decorators) → IDL TOML (plan 6.5
/// migration). Each `class X extends Schema` becomes `[types.X]`; `"string"` → `string`,
/// `"number"`/`float32`/`float64` → `float`, `int*`/`uint*` → `int`, `"boolean"` → `bool`,
/// `Other` → `Other`, `[T]` / `ArraySchema<T>` → `list<T>`, `{ map: T }` / `MapSchema<T>` →
/// `map<T>`. `root` names the state class (`[matches.<module>] state = Root`); when absent the
/// class no other class references is used.
pub fn from_colyseus(ts: &str, module: &str, root: Option<&str>) -> Result<String> {
    use std::collections::BTreeSet;
    let mut classes: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    let bytes = ts.as_bytes();
    while let Some(off) = ts[i..].find("class ") {
        let start = i + off + 6;
        let rest = &ts[start..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let Some(brace) = rest.find('{') else { break };
        let header = &rest[..brace];
        if name.is_empty() || !header.contains("extends") {
            i = start;
            continue;
        }
        // body: match braces
        let mut depth = 0usize;
        let mut end = None;
        for (k, c) in rest[brace..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(brace + k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let body = &rest[brace + 1..end];
        let mut fields = Vec::new();
        let mut j = 0;
        while let Some(at) = body[j..].find("@type(") {
            let a = j + at + 6;
            let Some(close) = body[a..].find(')') else {
                break;
            };
            let spec = body[a..a + close].trim();
            let after = &body[a + close + 1..];
            // field name: first identifier after the decorator (skipping modifiers)
            let mut fname = String::new();
            for tok in after
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|t| !t.is_empty())
            {
                if matches!(
                    tok,
                    "public" | "private" | "protected" | "readonly" | "declare"
                ) {
                    continue;
                }
                fname = tok.to_string();
                break;
            }
            let ty = colyseus_type(spec, &mut referenced, false)?;
            if !fname.is_empty() {
                fields.push((fname, ty));
            }
            j = a + close + 1;
        }
        classes.push((name, fields));
        i = start + end;
        let _ = bytes;
    }
    if classes.is_empty() {
        return Err(IdlError(
            "no `class X extends Schema` with @type fields found".into(),
        ));
    }
    let root = match root {
        Some(r) => r.to_string(),
        None => classes
            .iter()
            .map(|(n, _)| n.clone())
            .find(|n| !referenced.contains(n))
            .unwrap_or_else(|| classes[0].0.clone()),
    };
    let mut out = String::new();
    out.push_str("# generated from Colyseus Schema classes by `musubi idl from-colyseus`\n\n");
    for (name, fields) in &classes {
        out.push_str(&format!("[types.{name}]\n"));
        for (f, t) in fields {
            out.push_str(&format!("{f} = \"{t}\"\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("[matches.{module}]\nstate = \"{root}\"\n"));
    Idl::parse(&out)?;
    Ok(out)
}

fn colyseus_type(
    spec: &str,
    referenced: &mut std::collections::BTreeSet<String>,
    nested: bool,
) -> Result<String> {
    let s = spec.trim();
    let scalar = |n: &str| -> Option<&'static str> {
        Some(match n {
            "string" => "string",
            "number" | "float32" | "float64" => "float",
            "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64"
            | "varint" => "int",
            "boolean" => "bool",
            _ => return None,
        })
    };
    if let Some(q) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return scalar(q)
            .map(str::to_string)
            .ok_or_else(|| IdlError(format!("unknown Colyseus type \"{q}\"")));
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return Ok(format!("list<{}>", colyseus_type(inner, referenced, true)?));
    }
    if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        let (k, v) = inner
            .split_once(':')
            .ok_or_else(|| IdlError(format!("bad collection type {s}")))?;
        let elem = colyseus_type(v.trim(), referenced, true)?;
        return Ok(match k.trim() {
            "map" => format!("map<{elem}>"),
            "array" | "set" | "collection" => format!("list<{elem}>"),
            other => return Err(IdlError(format!("unsupported Colyseus collection {other}"))),
        });
    }
    if s.chars().all(|c| c.is_alphanumeric() || c == '_') && !s.is_empty() {
        referenced.insert(s.to_string());
        // a bare reference may be unset in Colyseus; inside collections elements always exist
        return Ok(if nested {
            s.to_string()
        } else {
            format!("{s}?")
        });
    }
    Err(IdlError(format!("cannot map Colyseus type {s}")))
}

#[cfg(test)]
mod colyseus_tests {
    use super::*;

    #[test]
    fn converts_schema_classes() {
        let ts = r#"
import { Schema, type, MapSchema, ArraySchema } from "@colyseus/schema";

export class Card extends Schema {
  @type("uint8") rank: number = 0;
  @type("string") suit: string = "";
}

export class Player extends Schema {
  @type("string") public name: string;
  @type("number") hp = 100;
  @type("boolean") ready: boolean = false;
  @type([Card]) hand = new ArraySchema<Card>();
  @type(["string"]) tags = new ArraySchema<string>();
}

export class GameState extends Schema {
  @type({ map: Player }) players = new MapSchema<Player>();
  @type(Player) winner: Player;
  @type("int32") round: number = 1;
}
"#;
        let out = from_colyseus(ts, "cards", None).unwrap();
        assert!(
            out.contains("[types.Card]\nrank = \"int\"\nsuit = \"string\"\n"),
            "{out}"
        );
        assert!(out.contains("hand = \"list<Card>\""), "{out}");
        assert!(out.contains("tags = \"list<string>\""), "{out}");
        assert!(out.contains("players = \"map<Player>\""), "{out}");
        assert!(out.contains("winner = \"Player?\""), "{out}");
        assert!(
            out.contains("[matches.cards]\nstate = \"GameState\"\n"),
            "{out}"
        );
        let idl = Idl::parse(&out).unwrap();
        assert!(idl.match_state_schema("cards").is_some());
        assert_eq!(idl.match_sync("cards"), Some(vec![]));
        let forced = from_colyseus(ts, "m", Some("Player")).unwrap();
        assert!(forced.contains("state = \"Player\""));
        assert!(from_colyseus("const x = 1;", "m", None).is_err());
        assert!(from_colyseus("class A extends Schema { @type(\"vec3\") p; }", "m", None).is_err());
    }
}
