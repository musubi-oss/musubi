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
        self.rpcs
            .iter()
            .map(|(name, rpc)| {
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
            })
            .collect()
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
}
