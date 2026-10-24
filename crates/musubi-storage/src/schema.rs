//! Per-collection contracts for player data: a JSON Schema subset the server enforces on write,
//! versioned migrations applied lazily on read (and in bulk from the console), secondary
//! expression indexes, and the `$where` range filter that uses them.

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_INDEXES: usize = 16;
pub const MAX_WHERE_OPS: usize = 8;
/// Nesting cap shared by schema compilation and validation.
const MAX_SCHEMA_DEPTH: usize = 64;
/// Compiled-size cap for `pattern` regexes (operator-defined, but compiled on every write).
const PATTERN_SIZE_LIMIT: usize = 1 << 18;

fn compile_pattern(p: &str) -> std::result::Result<regex::Regex, regex::Error> {
    regex::RegexBuilder::new(p)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionSchema {
    pub collection: String,
    /// Current schema version; objects remember the version they were written with.
    #[serde(default)]
    pub version: i32,
    /// JSON Schema (draft 2020-12 subset); `null` / `{}` validates everything.
    #[serde(default)]
    pub schema: Value,
    #[serde(default)]
    pub migrations: Vec<Migration>,
    #[serde(default)]
    pub indexes: Vec<IndexDef>,
    /// Access policy expressions (plan 3.5) for user callers; see `policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<crate::policy::Policy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Migration {
    pub from: i32,
    pub to: i32,
    #[serde(default)]
    pub ops: Vec<MigrationOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MigrationOp {
    /// Always write `value` at `path` (intermediate objects are created).
    Set {
        path: String,
        value: Value,
    },
    /// Write `value` at `path` only when nothing is there.
    Default {
        path: String,
        value: Value,
    },
    Rename {
        from: String,
        to: String,
    },
    Remove {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexDef {
    /// Dotted field of the value (`score`, `stats.wins`).
    pub field: String,
    /// `text` (default) or `number`.
    #[serde(default = "text")]
    pub kind: String,
    #[serde(default)]
    pub unique: bool,
}

fn text() -> String {
    "text".into()
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhereOp {
    pub path: Vec<String>,
    pub op: &'static str,
    pub kind: WhereKind,
    /// Bound as text and cast in SQL (`::numeric` / `::jsonb`).
    pub value: String,
    /// `0` = top-level `and`; `n > 0` = member of the n-th `$or` group.
    pub group: u32,
    /// Alternative within the `$or` group; conditions sharing `(group, alt)` are `and`-ed.
    pub alt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhereKind {
    Number,
    Text,
    Bool,
    /// `"$q"`: websearch-style full-text query over the whole value (`path` unused).
    FullText,
    /// `in` / `nin`: value is a JSON array; scalar membership through jsonb containment.
    In,
    NotIn,
    /// `like` / `ilike` / `prefix` (prefix is escaped into a `like` pattern).
    Like,
    ILike,
    /// `regex` (`~`) / `iregex` (`~*`): POSIX regular expression over the text value.
    Regex,
    IRegex,
    /// `exists`: whether the field is present (value `true` / `false`).
    Exists,
    /// `contains` (`@>`): jsonb containment — array has element(s), object has pairs.
    Contains,
    /// `size`, `size>=`, `size<=`: array length (`op` carries the comparison).
    Size,
    /// `between`: `[lo, hi]` numbers, inclusive.
    Between,
}

pub fn parse_path(field: &str) -> Result<Vec<String>> {
    let path: Vec<String> = field.split('.').map(str::to_string).collect();
    let ok = !path.is_empty()
        && path.len() <= 8
        && path.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 64
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    if !ok {
        return Err(Error::InvalidArgument(format!(
            "bad field path {field:?}: dotted [A-Za-z0-9_] segments, at most 8"
        )));
    }
    Ok(path)
}

pub fn pg_path(path: &[String]) -> String {
    format!("'{{{}}}'", path.join(","))
}

fn pg_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn index_name(collection: &str, def: &IndexDef) -> String {
    let h = Sha256::digest(format!(
        "{collection}\0{}\0{}\0{}",
        def.field, def.kind, def.unique
    ));
    format!("storage_x_{}", hex::encode(&h[..12]))
}

pub fn index_ddl(collection: &str, def: &IndexDef) -> Result<String> {
    let path = parse_path(&def.field)?;
    let expr = match def.kind.as_str() {
        "text" => format!("((value #>> {}))", pg_path(&path)),
        "number" => format!("(((value #>> {})::numeric))", pg_path(&path)),
        k => {
            return Err(Error::InvalidArgument(format!(
                "index kind {k:?}: expected text|number"
            )))
        }
    };
    Ok(format!(
        "create {}index if not exists {} on musubi.storage_objects {expr} where collection = {}",
        if def.unique { "unique " } else { "" },
        index_name(collection, def),
        pg_literal(collection)
    ))
}

/// Pull `"$where": [[field, op, value], …]` out of a containment filter.
pub fn split_where(filter: Option<&Value>) -> Result<(Option<Value>, Vec<WhereOp>)> {
    let Some(f) = filter else {
        return Ok((None, vec![]));
    };
    let Some(obj) = f.as_object() else {
        return Err(Error::InvalidArgument("where must be a JSON object".into()));
    };
    let mut ops = Vec::new();
    if let Some(q) = obj.get("$q") {
        let q = q.as_str().map(str::trim).unwrap_or_default();
        if q.is_empty() || q.len() > 256 {
            return Err(Error::InvalidArgument(
                "$q must be a non-empty string of at most 256 chars".into(),
            ));
        }
        ops.push(WhereOp {
            path: vec![],
            op: "@@",
            kind: WhereKind::FullText,
            value: q.to_string(),
            group: 0,
            alt: 0,
        });
    }
    let empty = Vec::new();
    let items: &Vec<Value> = match obj.get("$where") {
        None => &empty,
        Some(raw) => raw.as_array().ok_or_else(|| {
            Error::InvalidArgument("$where must be an array of [field, op, value]".into())
        })?,
    };
    if items.len() > MAX_WHERE_OPS {
        return Err(Error::InvalidArgument(format!(
            "$where: at most {MAX_WHERE_OPS} conditions"
        )));
    }
    ops.reserve(items.len());
    for it in items {
        ops.push(parse_cond(it, 0, 0)?);
    }
    if let Some(or) = obj.get("$or") {
        let groups = or
            .as_array()
            .filter(|g| !g.is_empty() && g.len() <= 16)
            .ok_or_else(|| {
                Error::InvalidArgument(
                    "$or must be a non-empty array of up to 16 alternatives".into(),
                )
            })?;
        for (alt, g) in groups.iter().enumerate() {
            let conds: Vec<&Value> = match g.as_array() {
                // a single [field, op, value] triple or a list of triples (and-ed)
                Some(a) if a.first().is_some_and(|x| x.is_string()) => vec![g],
                Some(a) if !a.is_empty() => a.iter().collect(),
                _ => {
                    return Err(Error::InvalidArgument(format!(
                        "$or: alternative {alt} must be a condition or a list of conditions"
                    )))
                }
            };
            for c in conds {
                ops.push(parse_cond(c, 1, alt as u32)?);
            }
        }
        if ops.len() > MAX_WHERE_OPS {
            return Err(Error::InvalidArgument(format!(
                "$where/$or: at most {MAX_WHERE_OPS} conditions"
            )));
        }
    }
    let mut rest = obj.clone();
    rest.remove("$where");
    rest.remove("$or");
    rest.remove("$q");
    let rest = if rest.is_empty() {
        None
    } else {
        Some(Value::Object(rest))
    };
    Ok((rest, ops))
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// One `[field, op, value]` condition.
fn parse_cond(it: &Value, group: u32, alt: u32) -> Result<WhereOp> {
    let bad = || Error::InvalidArgument(format!("$where: bad condition {it}"));
    let arr = it.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
    let path = parse_path(arr[0].as_str().ok_or_else(bad)?)?;
    let op_s = arr[1].as_str().ok_or_else(bad)?;
    let v = &arr[2];
    let scalar = |v: &Value| -> Result<(WhereKind, String)> {
        match v {
            Value::Number(n) => Ok((WhereKind::Number, n.to_string())),
            Value::String(s) => Ok((WhereKind::Text, s.clone())),
            Value::Bool(b) => Ok((WhereKind::Bool, b.to_string())),
            _ => Err(bad()),
        }
    };
    let text = |v: &Value| -> Result<String> {
        let s = v.as_str().ok_or_else(bad)?;
        if s.is_empty() || s.len() > 256 {
            return Err(bad());
        }
        Ok(s.to_string())
    };
    // Postgres' ARE engine is linear except for back-references, which make it backtrack
    // exponentially; a player-supplied `\1` could pin a backend for minutes.
    let regex = |v: &Value| -> Result<String> {
        let s = text(v)?;
        if has_backreference(&s) {
            return Err(bad());
        }
        Ok(s)
    };
    let (op, kind, value): (&'static str, WhereKind, String) = match op_s {
        "=" | "==" | "!=" | "<" | "<=" | ">" | ">=" => {
            let op = match op_s {
                "=" | "==" => "=",
                "!=" => "<>",
                o => match o {
                    "<" => "<",
                    "<=" => "<=",
                    ">" => ">",
                    _ => ">=",
                },
            };
            let (kind, value) = scalar(v)?;
            if kind == WhereKind::Bool && op != "=" && op != "<>" {
                return Err(bad());
            }
            (op, kind, value)
        }
        "in" | "nin" | "not in" => {
            let a = v
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 100)
                .ok_or_else(bad)?;
            if !a
                .iter()
                .all(|x| x.is_number() || x.is_string() || x.is_boolean())
            {
                return Err(bad());
            }
            let kind = if op_s == "in" {
                WhereKind::In
            } else {
                WhereKind::NotIn
            };
            ("", kind, v.to_string())
        }
        "like" => ("like", WhereKind::Like, text(v)?),
        "ilike" => ("ilike", WhereKind::ILike, text(v)?),
        "prefix" => (
            "like",
            WhereKind::Like,
            format!("{}%", escape_like(&text(v)?)),
        ),
        "regex" | "~" => ("~", WhereKind::Regex, regex(v)?),
        "iregex" | "~*" => ("~*", WhereKind::IRegex, regex(v)?),
        "exists" => (
            "",
            WhereKind::Exists,
            v.as_bool().ok_or_else(bad)?.to_string(),
        ),
        "contains" | "@>" => {
            if v.is_null() {
                return Err(bad());
            }
            ("", WhereKind::Contains, v.to_string())
        }
        "size" | "size>=" | "size<=" | "size>" | "size<" => {
            let n = v.as_i64().filter(|n| *n >= 0).ok_or_else(bad)?;
            let op = match op_s {
                "size" => "=",
                "size>=" => ">=",
                "size<=" => "<=",
                "size>" => ">",
                _ => "<",
            };
            (op, WhereKind::Size, n.to_string())
        }
        "between" => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(bad)?;
            if !a.iter().all(Value::is_number) {
                return Err(bad());
            }
            ("", WhereKind::Between, v.to_string())
        }
        _ => return Err(bad()),
    };
    Ok(WhereOp {
        path,
        op,
        kind,
        value,
        group,
        alt,
    })
}

/// `\1`..`\9` anywhere in a POSIX regex (a `\\1` literal is rejected too; acceptable loss).
fn has_backreference(re: &str) -> bool {
    re.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'\\' && w[1].is_ascii_digit())
}

/// SQL expression of one condition with its value bound as `$p`.
fn cond_sql(o: &WhereOp, p: usize) -> String {
    let path = if o.path.is_empty() {
        String::new()
    } else {
        pg_path(&o.path)
    };
    let num = format!(
        "(case when jsonb_typeof(value #> {path}) = 'number' then (value #>> {path})::numeric end)"
    );
    match o.kind {
        // CASE guarantees the cast only runs on numbers (a text value would abort the query)
        WhereKind::Number => format!("{num} {} ${p}::numeric", o.op),
        WhereKind::Text => format!("(value #>> {path}) {} ${p}::text", o.op),
        WhereKind::Bool => format!("(value #> {path}) {} ${p}::jsonb", o.op),
        WhereKind::FullText => format!(
            "to_tsvector('simple', value) @@ websearch_to_tsquery('simple', ${p}::text)"
        ),
        WhereKind::In => format!("coalesce(value #> {path}, 'null'::jsonb) <@ ${p}::jsonb"),
        WhereKind::NotIn => format!("not (coalesce(value #> {path}, 'null'::jsonb) <@ ${p}::jsonb)"),
        WhereKind::Like | WhereKind::ILike | WhereKind::Regex | WhereKind::IRegex => {
            format!("(value #>> {path}) {} ${p}::text", o.op)
        }
        WhereKind::Exists => format!("((value #> {path}) is not null) = (${p}::text = 'true')"),
        WhereKind::Contains => format!("(value #> {path}) @> ${p}::jsonb"),
        WhereKind::Size => format!(
            "(case when jsonb_typeof(value #> {path}) = 'array' then jsonb_array_length(value #> {path}) end) {} ${p}::int",
            o.op
        ),
        WhereKind::Between => format!(
            "{num} between (${p}::jsonb->>0)::numeric and (${p}::jsonb->>1)::numeric"
        ),
    }
}

/// `and …` clauses for `ops`, binding their values as `$first_param, $first_param+1, …` (text).
/// Top-level conditions are `and`-ed; `$or` members become `and ((a1 and a2) or (b1) …)`.
pub fn where_sql(ops: &[WhereOp], first_param: usize) -> String {
    let mut sql = String::new();
    let mut groups: std::collections::BTreeMap<u32, std::collections::BTreeMap<u32, Vec<String>>> =
        Default::default();
    for (i, o) in ops.iter().enumerate() {
        let expr = cond_sql(o, first_param + i);
        if o.group == 0 {
            sql.push_str(&format!(" and {expr}"));
        } else {
            groups
                .entry(o.group)
                .or_default()
                .entry(o.alt)
                .or_default()
                .push(expr);
        }
    }
    for alts in groups.values() {
        let parts: Vec<String> = alts
            .values()
            .map(|c| format!("({})", c.join(" and ")))
            .collect();
        sql.push_str(&format!(" and ({})", parts.join(" or ")));
    }
    sql
}

/// Check a schema document before it is stored.
pub fn validate_definition(s: &CollectionSchema) -> Result<()> {
    if let Some(p) = &s.policy {
        p.validate()?;
    }
    if s.version < 0 {
        return Err(Error::InvalidArgument("version must be ≥ 0".into()));
    }
    if !(s.schema.is_null() || s.schema.is_object()) {
        return Err(Error::InvalidArgument("schema must be an object".into()));
    }
    compile(&s.schema, &s.schema, "schema")?;
    for m in &s.migrations {
        if m.to <= m.from || m.to > s.version {
            return Err(Error::InvalidArgument(format!(
                "migration {}→{}: must move forward and not past version {}",
                m.from, m.to, s.version
            )));
        }
        if s.migrations.iter().filter(|o| o.from == m.from).count() > 1 {
            return Err(Error::InvalidArgument(format!(
                "two migrations start at version {}",
                m.from
            )));
        }
        for op in &m.ops {
            match op {
                MigrationOp::Set { path, .. }
                | MigrationOp::Default { path, .. }
                | MigrationOp::Remove { path } => {
                    parse_path(path)?;
                }
                MigrationOp::Rename { from, to } => {
                    parse_path(from)?;
                    parse_path(to)?;
                }
            }
        }
    }
    if s.indexes.len() > MAX_INDEXES {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_INDEXES} indexes per collection"
        )));
    }
    for d in &s.indexes {
        index_ddl(&s.collection, d)?;
    }
    Ok(())
}

/// Bring `value` written at `from` as far forward as the migration chain allows; returns the
/// version reached.
pub fn migrate(s: &CollectionSchema, value: &mut Value, from: i32) -> i32 {
    migrate_steps(&s.migrations, s.version, value, from)
}

/// Compile a bare JSON schema (the subset accepted for collections) and reject unknown keywords.
pub fn compile_schema(schema: &Value) -> Result<()> {
    if !schema.is_object() {
        return Err(Error::InvalidArgument("schema must be an object".into()));
    }
    compile(schema, schema, "schema")
}

/// Migrations must move forward, not past `version`, and start at distinct versions.
pub fn check_migrations(migrations: &[Migration], version: i32) -> Result<()> {
    for m in migrations {
        if m.to <= m.from || m.to > version {
            return Err(Error::InvalidArgument(format!(
                "migration {}→{}: must move forward and not past version {}",
                m.from, m.to, version
            )));
        }
        if migrations.iter().filter(|o| o.from == m.from).count() > 1 {
            return Err(Error::InvalidArgument(format!(
                "two migrations start at version {}",
                m.from
            )));
        }
    }
    Ok(())
}

/// Walk `value` from version `from` towards `target` along `migrations`; returns the version reached
/// (the chain may stop short when a step is missing).
pub fn migrate_steps(migrations: &[Migration], target: i32, value: &mut Value, from: i32) -> i32 {
    let mut v = from;
    let mut hops = 0;
    while v < target && hops < 1024 {
        let Some(m) = migrations.iter().find(|m| m.from == v) else {
            break;
        };
        for op in &m.ops {
            apply(value, op);
        }
        v = m.to;
        hops += 1;
    }
    v
}

fn apply(value: &mut Value, op: &MigrationOp) {
    match op {
        MigrationOp::Set { path, value: nv } => set_path(value, path, nv.clone()),
        MigrationOp::Default { path, value: nv } => {
            if get_path(value, path).is_none() {
                set_path(value, path, nv.clone());
            }
        }
        MigrationOp::Rename { from, to } => {
            if let Some(v) = remove_path(value, from) {
                set_path(value, to, v);
            }
        }
        MigrationOp::Remove { path } => {
            remove_path(value, path);
        }
    }
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, k| cur.get(k))
}

fn set_path(v: &mut Value, path: &str, nv: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = v;
    for k in &parts[..parts.len() - 1] {
        if !cur.is_object() {
            *cur = Value::Object(Default::default());
        }
        cur = cur
            .as_object_mut()
            .unwrap()
            .entry(k.to_string())
            .or_insert(Value::Object(Default::default()));
    }
    if !cur.is_object() {
        *cur = Value::Object(Default::default());
    }
    cur.as_object_mut()
        .unwrap()
        .insert(parts[parts.len() - 1].to_string(), nv);
}

fn remove_path(v: &mut Value, path: &str) -> Option<Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = v;
    for k in &parts[..parts.len() - 1] {
        cur = cur.get_mut(*k)?;
    }
    cur.as_object_mut()?.remove(parts[parts.len() - 1])
}

const KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "definitions",
    "$ref",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "anyOf",
    "oneOf",
    "allOf",
    "not",
    "format",
];

const TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "object", "array", "null",
];

/// Reject unknown keywords / malformed constraints up front so a typo cannot silently validate
/// everything (or nothing).
fn compile(root: &Value, s: &Value, at: &str) -> Result<()> {
    compile_at(root, s, at, 0)
}

fn compile_at(root: &Value, s: &Value, at: &str, depth: usize) -> Result<()> {
    let bad = |m: String| Error::InvalidArgument(format!("{at}: {m}"));
    if depth > MAX_SCHEMA_DEPTH {
        return Err(bad(format!("schema nested deeper than {MAX_SCHEMA_DEPTH}")));
    }
    let obj = match s {
        Value::Null => return Ok(()),
        Value::Bool(_) => return Ok(()),
        Value::Object(o) => o,
        _ => return Err(bad("schema must be an object or boolean".into())),
    };
    for (k, v) in obj {
        if !KEYWORDS.contains(&k.as_str()) {
            return Err(bad(format!("unknown keyword {k:?}")));
        }
        match k.as_str() {
            "type" => {
                let names: Vec<&str> = match v {
                    Value::String(t) => vec![t.as_str()],
                    Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                    _ => return Err(bad("type must be a string or array".into())),
                };
                if let Some(t) = names.iter().find(|t| !TYPES.contains(t)) {
                    return Err(bad(format!("unknown type {t:?}")));
                }
            }
            "properties" | "$defs" | "definitions" => {
                let Some(m) = v.as_object() else {
                    return Err(bad(format!("{k} must be an object")));
                };
                for (name, sub) in m {
                    compile_at(root, sub, &format!("{at}.{k}.{name}"), depth + 1)?;
                }
            }
            "items" | "additionalProperties" | "not" => {
                compile_at(root, v, &format!("{at}.{k}"), depth + 1)?;
            }
            "anyOf" | "oneOf" | "allOf" => {
                let Some(a) = v.as_array() else {
                    return Err(bad(format!("{k} must be an array")));
                };
                for (i, sub) in a.iter().enumerate() {
                    compile_at(root, sub, &format!("{at}.{k}[{i}]"), depth + 1)?;
                }
            }
            "required" => {
                if !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)) {
                    return Err(bad("required must be an array of strings".into()));
                }
            }
            "enum" => {
                if !v.is_array() {
                    return Err(bad("enum must be an array".into()));
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf" => {
                if !v.is_number() {
                    return Err(bad(format!("{k} must be a number")));
                }
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
            | "maxProperties" => {
                if !v.is_u64() {
                    return Err(bad(format!("{k} must be a non-negative integer")));
                }
            }
            "uniqueItems" => {
                if !v.is_boolean() {
                    return Err(bad("uniqueItems must be a boolean".into()));
                }
            }
            "pattern" => {
                let p = v
                    .as_str()
                    .ok_or_else(|| bad("pattern must be a string".into()))?;
                compile_pattern(p).map_err(|e| bad(format!("bad pattern: {e}")))?;
            }
            "$ref" => {
                let r = v
                    .as_str()
                    .ok_or_else(|| bad("$ref must be a string".into()))?;
                resolve_ref(root, r).ok_or_else(|| bad(format!("unresolvable {r:?}")))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn resolve_ref<'a>(root: &'a Value, r: &str) -> Option<&'a Value> {
    let rest = r.strip_prefix("#/")?;
    rest.split('/').try_fold(root, |cur, k| cur.get(k))
}

/// Validate `value` against `schema`; the error names the offending path.
pub fn validate(schema: &Value, value: &Value) -> Result<()> {
    let mut errs = Vec::new();
    check(schema, schema, value, "value", &mut errs, 0);
    if errs.is_empty() {
        Ok(())
    } else {
        errs.truncate(8);
        Err(Error::InvalidArgument(errs.join("; ")))
    }
}

fn push_err(errs: &mut Vec<String>, at: &str, m: String) {
    errs.push(format!("{at}: {m}"));
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_type(v: &Value, t: &str) -> bool {
    match t {
        "integer" => v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => type_name(v) == t,
    }
}

fn check(root: &Value, s: &Value, v: &Value, at: &str, errs: &mut Vec<String>, depth: usize) {
    if depth > MAX_SCHEMA_DEPTH || errs.len() >= 8 {
        return;
    }
    let obj = match s {
        Value::Object(o) => o,
        Value::Bool(false) => {
            errs.push(format!("{at}: not allowed"));
            return;
        }
        _ => return,
    };
    if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
        if let Some(target) = resolve_ref(root, r) {
            check(root, target, v, at, errs, depth + 1);
        }
    }
    if let Some(t) = obj.get("type") {
        let ok = match t {
            Value::String(t) => is_type(v, t),
            Value::Array(a) => a.iter().filter_map(Value::as_str).any(|t| is_type(v, t)),
            _ => true,
        };
        if !ok {
            push_err(errs, at, format!("expected {t}, got {}", type_name(v)));
            return;
        }
    }
    if let Some(e) = obj.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            push_err(errs, at, "not one of the allowed values".into());
        }
    }
    if let Some(c) = obj.get("const") {
        if c != v {
            push_err(errs, at, format!("must equal {c}"));
        }
    }
    if let Some(n) = v.as_f64() {
        for (k, test) in [
            ("minimum", (|n: f64, b: f64| n >= b) as fn(f64, f64) -> bool),
            ("maximum", |n, b| n <= b),
            ("exclusiveMinimum", |n, b| n > b),
            ("exclusiveMaximum", |n, b| n < b),
        ] {
            if let Some(b) = obj.get(k).and_then(Value::as_f64) {
                if !test(n, b) {
                    push_err(errs, at, format!("{k} {b} violated by {n}"));
                }
            }
        }
        if let Some(m) = obj.get("multipleOf").and_then(Value::as_f64) {
            if m > 0.0 && ((n / m).round() - n / m).abs() > 1e-9 {
                push_err(errs, at, format!("not a multiple of {m}"));
            }
        }
    }
    if let Some(sv) = v.as_str() {
        let len = sv.chars().count() as u64;
        if let Some(m) = obj.get("minLength").and_then(Value::as_u64) {
            if len < m {
                push_err(errs, at, format!("shorter than {m}"));
            }
        }
        if let Some(m) = obj.get("maxLength").and_then(Value::as_u64) {
            if len > m {
                push_err(errs, at, format!("longer than {m}"));
            }
        }
        if let Some(p) = obj.get("pattern").and_then(Value::as_str) {
            if let Ok(re) = compile_pattern(p) {
                if !re.is_match(sv) {
                    push_err(errs, at, format!("does not match {p:?}"));
                }
            }
        }
    }
    if let Some(arr) = v.as_array() {
        if let Some(m) = obj.get("minItems").and_then(Value::as_u64) {
            if (arr.len() as u64) < m {
                push_err(errs, at, format!("fewer than {m} items"));
            }
        }
        if let Some(m) = obj.get("maxItems").and_then(Value::as_u64) {
            if (arr.len() as u64) > m {
                push_err(errs, at, format!("more than {m} items"));
            }
        }
        if obj.get("uniqueItems") == Some(&Value::Bool(true)) {
            // `Value` serializes objects with sorted keys, so the JSON text is a canonical form
            // and this stays O(n) for the 1 MiB arrays a player may submit.
            let mut seen = std::collections::HashSet::with_capacity(arr.len());
            let unique = arr.iter().all(|a| seen.insert(a.to_string()));
            if !unique {
                push_err(errs, at, "items must be unique".into());
            }
        }
        if let Some(items) = obj.get("items") {
            for (i, item) in arr.iter().enumerate() {
                check(root, items, item, &format!("{at}[{i}]"), errs, depth + 1);
            }
        }
    }
    if let Some(map) = v.as_object() {
        if let Some(req) = obj.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !map.contains_key(r) {
                    push_err(errs, at, format!("missing required {r:?}"));
                }
            }
        }
        if let Some(m) = obj.get("minProperties").and_then(Value::as_u64) {
            if (map.len() as u64) < m {
                push_err(errs, at, format!("fewer than {m} properties"));
            }
        }
        if let Some(m) = obj.get("maxProperties").and_then(Value::as_u64) {
            if (map.len() as u64) > m {
                push_err(errs, at, format!("more than {m} properties"));
            }
        }
        let props = obj.get("properties").and_then(Value::as_object);
        for (k, pv) in map {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(root, ps, pv, &format!("{at}.{k}"), errs, depth + 1),
                None => match obj.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        errs.push(format!("{at}.{k}: unexpected property"));
                    }
                    Some(sub @ Value::Object(_)) => {
                        check(root, sub, pv, &format!("{at}.{k}"), errs, depth + 1)
                    }
                    _ => {}
                },
            }
        }
    }
    if let Some(all) = obj.get("allOf").and_then(Value::as_array) {
        for sub in all {
            check(root, sub, v, at, errs, depth + 1);
        }
    }
    let passes = |sub: &Value| {
        let mut e = Vec::new();
        check(root, sub, v, at, &mut e, depth + 1);
        e.is_empty()
    };
    if let Some(any) = obj.get("anyOf").and_then(Value::as_array) {
        if !any.iter().any(passes) {
            errs.push(format!("{at}: matches none of anyOf"));
        }
    }
    if let Some(one) = obj.get("oneOf").and_then(Value::as_array) {
        if one.iter().filter(|s| passes(s)).count() != 1 {
            errs.push(format!("{at}: must match exactly one of oneOf"));
        }
    }
    if let Some(not) = obj.get("not") {
        if passes(not) {
            errs.push(format!("{at}: must not match schema"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_subset() {
        let s = json!({
            "type": "object",
            "required": ["name", "level"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 2, "pattern": "^[a-z]+$"},
                "level": {"type": "integer", "minimum": 1, "maximum": 60},
                "tags": {"type": "array", "items": {"type": "string"}, "uniqueItems": true, "maxItems": 3},
                "cls": {"enum": ["mage", "rogue"]},
                "pos": {"$ref": "#/$defs/vec"}
            },
            "$defs": {"vec": {"type": "object", "properties": {"x": {"type": "number"}}, "required": ["x"]}}
        });
        compile(&s, &s, "schema").unwrap();
        validate(
            &s,
            &json!({"name": "bob", "level": 3, "tags": ["a"], "cls": "mage", "pos": {"x": 1.5}}),
        )
        .unwrap();
        let e = validate(&s, &json!({"name": "B", "level": 61, "tags": ["a", "a"], "cls": "bard", "extra": 1, "pos": {}}))
            .unwrap_err()
            .to_string();
        for m in [
            "value.name",
            "value.level",
            "value.tags",
            "value.cls",
            "unexpected property",
        ] {
            assert!(e.contains(m), "{e} lacks {m}");
        }
        assert!(validate(&s, &json!({"name": "bob", "level": 1.5})).is_err());
        assert!(validate(&s, &json!({"name": "bob", "level": 2, "pos": {"y": 1}})).is_err());
        assert!(compile(&s, &json!({"typo": 1}), "schema").is_err());
        assert!(compile(&s, &json!({"type": "str"}), "schema").is_err());
        assert!(compile(&s, &json!({"pattern": "("}), "schema").is_err());
        assert!(compile(&s, &json!({"$ref": "#/$defs/nope"}), "schema").is_err());
    }

    #[test]
    fn migrates_in_steps() {
        let s = CollectionSchema {
            collection: "c".into(),
            version: 3,
            schema: Value::Null,
            migrations: vec![
                Migration {
                    from: 1,
                    to: 2,
                    ops: vec![
                        MigrationOp::Rename {
                            from: "gold".into(),
                            to: "wallet.gold".into(),
                        },
                        MigrationOp::Default {
                            path: "wallet.gems".into(),
                            value: json!(0),
                        },
                    ],
                },
                Migration {
                    from: 2,
                    to: 3,
                    ops: vec![
                        MigrationOp::Remove {
                            path: "legacy".into(),
                        },
                        MigrationOp::Set {
                            path: "v".into(),
                            value: json!(3),
                        },
                    ],
                },
            ],
            indexes: vec![],
            policy: None,
        };
        validate_definition(&s).unwrap();
        let mut v = json!({"gold": 5, "legacy": true, "wallet": {"gems": 9}});
        assert_eq!(migrate(&s, &mut v, 1), 3);
        assert_eq!(v, json!({"wallet": {"gold": 5, "gems": 9}, "v": 3}));
        let mut v0 = json!({});
        assert_eq!(migrate(&s, &mut v0, 0), 0, "no path from 0 → stays");
        let mut cur = json!({"a": 1});
        assert_eq!(migrate(&s, &mut cur, 3), 3);
        assert_eq!(cur, json!({"a": 1}));
        let bad = CollectionSchema {
            migrations: vec![Migration {
                from: 2,
                to: 2,
                ops: vec![],
            }],
            ..s.clone()
        };
        assert!(validate_definition(&bad).is_err());
    }

    #[test]
    fn where_ops_and_index_ddl() {
        let (rest, ops) = split_where(Some(&json!({
            "cls": "mage",
            "$where": [["stats.wins", ">=", 10], ["name", "<", "m"], ["flags.vip", "=", true]]
        })))
        .unwrap();
        assert_eq!(rest, Some(json!({"cls": "mage"})));
        assert_eq!(ops.len(), 3);
        let sql = where_sql(&ops, 6);
        assert!(
            sql.contains("(case when jsonb_typeof(value #> '{stats,wins}') = 'number' then (value #>> '{stats,wins}')::numeric end) >= $6::numeric"),
            "{sql}"
        );
        assert!(sql.contains("(value #>> '{name}') < $7::text"), "{sql}");
        assert!(
            sql.contains("(value #> '{flags,vip}') = $8::jsonb"),
            "{sql}"
        );
        assert!(split_where(Some(&json!({"$where": [["a", "~", 1]]}))).is_err());
        assert!(split_where(Some(&json!({"$where": [["a b", "=", 1]]}))).is_err());
        let ddl = index_ddl(
            "heroes",
            &IndexDef {
                field: "stats.wins".into(),
                kind: "number".into(),
                unique: false,
            },
        )
        .unwrap();
        assert!(
            ddl.contains("(((value #>> '{stats,wins}')::numeric)) where collection = 'heroes'"),
            "{ddl}"
        );
        assert!(index_ddl(
            "c",
            &IndexDef {
                field: "x".into(),
                kind: "blob".into(),
                unique: false
            }
        )
        .is_err());
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deeply_nested_schema_is_rejected() {
        // serde_json stops parsing at 128 levels, so 100 is the deepest a request can carry;
        // (deeper literals also overflow the stack when dropped, so keep the fixture small)
        let mut s = json!({"type": "string"});
        for _ in 0..100 {
            s = json!({"type": "object", "properties": {"a": s}});
        }
        let e = compile_schema(&s).unwrap_err().to_string();
        assert!(e.contains("nested deeper"), "{e}");
        let mut s = json!({"type": "string"});
        for _ in 0..100 {
            s = json!({"anyOf": [s]});
        }
        assert!(compile_schema(&s).is_err());
        let mut s = json!({"type": "string"});
        for _ in 0..40 {
            s = json!({"items": s});
        }
        assert!(compile_schema(&s).is_ok());
    }

    #[test]
    fn unique_items_is_linear_and_order_insensitive_for_objects() {
        let s = json!({"type": "array", "uniqueItems": true});
        let big: Vec<Value> = (0..200_000).map(|i| json!(i)).collect();
        let t = std::time::Instant::now();
        validate(&s, &Value::Array(big.clone())).unwrap();
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t.elapsed()
        );
        let mut dup = big;
        dup.push(json!(7));
        assert!(validate(&s, &Value::Array(dup)).is_err());
        assert!(validate(&s, &json!([{"a": 1, "b": 2}, {"b": 2, "a": 1}])).is_err());
        assert!(validate(&s, &json!([1, 1.0, "1"])).is_ok());
    }

    #[test]
    fn where_regex_rejects_backreferences() {
        for bad in [r"(a)\1", r"^(.*)\1$", r"x\9"] {
            assert!(
                split_where(Some(&json!({"$where": [["name", "~", bad]]}))).is_err(),
                "{bad}"
            );
            assert!(split_where(Some(&json!({"$where": [["name", "iregex", bad]]}))).is_err());
        }
        let (_, ops) = split_where(Some(&json!({"$where": [["name", "~", "^a.*\\d+$"]]}))).unwrap();
        assert_eq!(ops[0].kind, WhereKind::Regex);
        assert!(has_backreference(r"\1") && !has_backreference(r"\d"));
    }

    #[test]
    fn pattern_size_is_capped() {
        let huge = format!("{}{{1000}}{{1000}}", "a");
        assert!(compile_schema(&json!({"pattern": huge})).is_err());
        assert!(compile_schema(&json!({"pattern": "^[a-z]{1,16}$"})).is_ok());
    }
}
