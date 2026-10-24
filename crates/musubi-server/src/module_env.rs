//! P1-63: typed per-module environment.
//!
//! A module declares what it needs in the IDL (`[env."shop/STRIPE_KEY"] type = "string",
//! required = true, secret = true`); operators bind values per deployment through
//! `/v1/admin/runtime/modules/{name}/env`, and module code reads them through the `env` host
//! interface — scoped to the calling module, so one module never sees another's keys. Secrets are
//! sealed at rest and redacted in the admin API; `[runtime.secrets]` keeps working as the global,
//! untyped fallback.

use std::collections::BTreeMap;

use musubi_core::{Error, Result};
use musubi_idl::EnvDef;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

pub const TYPES: &[&str] = &["string", "int", "float", "bool", "json"];

/// Declarations for one module, keyed by the bare `KEY` (IDL keys are `module/KEY`).
pub fn defs(s: &AppState, module: &str) -> BTreeMap<String, EnvDef> {
    let Some(idl) = s.idl.get() else {
        return BTreeMap::new();
    };
    let prefix = format!("{module}/");
    idl.env
        .iter()
        .filter_map(|(k, d)| {
            k.strip_prefix(&prefix)
                .map(|key| (key.to_string(), d.clone()))
        })
        .collect()
}

/// Every env declaration problem in the loaded IDL (unknown type, default of the wrong type, bad
/// key) — surfaced on IDL publish.
pub fn lint(idl: &musubi_idl::Idl) -> Vec<String> {
    let mut out = Vec::new();
    for (k, d) in &idl.env {
        let Some((module, key)) = k.split_once('/') else {
            out.push(format!("env `{k}`: key must be `module/KEY`"));
            continue;
        };
        if module.is_empty() || valid_key(key).is_err() {
            out.push(format!(
                "env `{k}`: key must be `module/KEY` (KEY = [A-Za-z_][A-Za-z0-9_]*)"
            ));
        }
        if !TYPES.contains(&d.ty.as_str()) {
            out.push(format!("env `{k}`: unknown type `{}`", d.ty));
            continue;
        }
        if let Some(def) = &d.default {
            if let Err(e) = coerce(&d.ty, def.clone()) {
                out.push(format!("env `{k}`: default {e}"));
            }
        }
    }
    out
}

pub fn valid_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let ok = match chars.next() {
        Some(c) => {
            (c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
                && key.len() <= 64
        }
        None => false,
    };
    if !ok {
        return Err(Error::InvalidArgument(format!(
            "env key `{key}` must match [A-Za-z_][A-Za-z0-9_]{{0,63}}"
        )));
    }
    Ok(())
}

/// Convert an operator-supplied value to the declared type (strings are accepted for every scalar
/// type so CLI/CSV-style input works).
pub fn coerce(ty: &str, v: Value) -> Result<Value> {
    let bad = |what: &str| Error::InvalidArgument(format!("expected {ty}, got {what}"));
    Ok(match ty {
        "string" => match v {
            Value::String(_) => v,
            Value::Number(n) => Value::String(n.to_string()),
            Value::Bool(b) => Value::String(b.to_string()),
            _ => return Err(bad("non-scalar")),
        },
        "int" => match &v {
            Value::Number(n) if n.is_i64() => v,
            Value::Number(n)
                if n.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_992.0) =>
            {
                json!(n.as_f64().unwrap_or(0.0) as i64)
            }
            Value::String(t) => json!(t
                .trim()
                .parse::<i64>()
                .map_err(|_| bad(&format!("`{t}`")))?),
            _ => return Err(bad("non-integer")),
        },
        "float" => match &v {
            Value::Number(_) => v,
            Value::String(t) => json!(t
                .trim()
                .parse::<f64>()
                .map_err(|_| bad(&format!("`{t}`")))?),
            _ => return Err(bad("non-number")),
        },
        "bool" => match &v {
            Value::Bool(_) => v,
            Value::String(t) => match t.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => json!(true),
                "false" | "0" | "no" | "off" => json!(false),
                _ => return Err(bad(&format!("`{t}`"))),
            },
            Value::Number(n) if n.as_i64() == Some(0) || n.as_i64() == Some(1) => {
                json!(n.as_i64() == Some(1))
            }
            _ => return Err(bad("non-boolean")),
        },
        "json" => v,
        other => {
            return Err(Error::InvalidArgument(format!(
                "unknown env type `{other}` (one of {})",
                TYPES.join("|")
            )))
        }
    })
}

struct Binding {
    value: Value,
    secret: bool,
    updated_by: String,
    updated_at: time::OffsetDateTime,
}

async fn bindings(s: &AppState, module: &str) -> Result<BTreeMap<String, Binding>> {
    let rows = sqlx::query(
        "select key, value, is_secret, updated_by, updated_at from musubi.module_env where module = $1",
    )
    .bind(module)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out = BTreeMap::new();
    for r in rows {
        let key: String = r.get("key");
        let raw: String = r.get("value");
        let plain = if musubi_core::secrets::is_sealed(&raw) {
            musubi_core::secrets::open(&raw)
                .map_err(|e| Error::Internal(format!("module env `{module}/{key}`: {e}")))?
        } else {
            raw
        };
        let value = serde_json::from_str(&plain).unwrap_or(Value::String(plain));
        out.insert(
            key,
            Binding {
                value,
                secret: r.get("is_secret"),
                updated_by: r.get("updated_by"),
                updated_at: r.get("updated_at"),
            },
        );
    }
    Ok(out)
}

/// Effective values for module code: bindings win, then IDL defaults. Secrets are included —
/// the module owns them.
pub async fn resolve(s: &AppState, module: &str) -> Result<BTreeMap<String, Value>> {
    let defs = defs(s, module);
    let mut out: BTreeMap<String, Value> = defs
        .iter()
        .filter_map(|(k, d)| d.default.clone().map(|v| (k.clone(), v)))
        .collect();
    for (k, b) in bindings(s, module).await? {
        out.insert(k, b.value);
    }
    Ok(out)
}

/// Required declarations with neither a binding nor a default.
pub async fn missing(s: &AppState, module: &str) -> Result<Vec<String>> {
    let defs = defs(s, module);
    let bound = bindings(s, module).await?;
    Ok(defs
        .iter()
        .filter(|(k, d)| d.required && d.default.is_none() && !bound.contains_key(*k))
        .map(|(k, _)| k.clone())
        .collect())
}

/// Refuse to publish a build whose required env is unbound (unless the operator forces it).
pub async fn check_publish(s: &AppState, module: &str, force: bool) -> Result<()> {
    let miss = missing(s, module).await?;
    if miss.is_empty() || force {
        return Ok(());
    }
    Err(Error::InvalidArgument(format!(
        "module `{module}` has unbound required env: {} (bind them via PUT /v1/admin/runtime/modules/{module}/env or pass ?force=true)",
        miss.join(", ")
    )))
}

/// Operator view: every declared key plus undeclared bindings, with `source` and redacted secrets.
pub async fn describe(s: &AppState, module: &str) -> Result<Value> {
    let defs = defs(s, module);
    let bound = bindings(s, module).await?;
    let mut keys: Vec<String> = defs.keys().cloned().collect();
    for k in bound.keys() {
        if !defs.contains_key(k) {
            keys.push(k.clone());
        }
    }
    let mut entries = Vec::new();
    let mut miss = Vec::new();
    for k in keys {
        let d = defs.get(&k);
        let b = bound.get(&k);
        let secret = d.map(|d| d.secret).unwrap_or(false) || b.map(|b| b.secret).unwrap_or(false);
        let (source, value) = match (b, d.and_then(|d| d.default.as_ref())) {
            (Some(b), _) => ("binding", Some(b.value.clone())),
            (None, Some(def)) => ("default", Some(def.clone())),
            (None, None) => ("missing", None),
        };
        let required = d.map(|d| d.required).unwrap_or(false);
        if required && source == "missing" {
            miss.push(k.clone());
        }
        let mut e = json!({
            "key": k,
            "type": d.map(|d| d.ty.as_str()).unwrap_or("json"),
            "declared": d.is_some(),
            "required": required,
            "secret": secret,
            "source": source,
            "doc": d.and_then(|d| d.doc.clone()),
        });
        e["value"] = match value {
            Some(_) if secret => json!("***"),
            Some(v) => v,
            None => Value::Null,
        };
        if let Some(b) = b {
            e["updated_by"] = json!(b.updated_by);
            e["updated_at"] = json!(b
                .updated_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default());
        }
        entries.push(e);
    }
    Ok(json!({
        "module": module,
        "env": entries,
        "missing": miss,
        "ok": miss.is_empty(),
    }))
}

/// `PUT …/env {values: {KEY: value}, secrets: [KEY]}` — upserts the given keys (others are kept),
/// typed by the IDL declaration; undeclared keys are stored as `json` (secret if listed).
pub async fn bind(
    s: &AppState,
    module: &str,
    values: &serde_json::Map<String, Value>,
    secrets: &[String],
    by: &str,
) -> Result<Value> {
    if values.is_empty() {
        return Err(Error::InvalidArgument("values must not be empty".into()));
    }
    let defs = defs(s, module);
    let mut staged = Vec::new();
    for (k, v) in values {
        valid_key(k)?;
        let d = defs.get(k);
        let ty = d.map(|d| d.ty.as_str()).unwrap_or("json");
        let v = coerce(ty, v.clone()).map_err(|e| Error::InvalidArgument(format!("`{k}`: {e}")))?;
        let secret = d.map(|d| d.secret).unwrap_or(false) || secrets.iter().any(|x| x == k);
        staged.push((k.clone(), v, secret));
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for (k, v, secret) in &staged {
        let text = v.to_string();
        let stored = if *secret {
            musubi_core::secrets::seal(&text)
        } else {
            text
        };
        sqlx::query(
            "insert into musubi.module_env (module, key, value, is_secret, updated_by, updated_at)
             values ($1, $2, $3, $4, $5, now())
             on conflict (module, key) do update set value = excluded.value,
               is_secret = excluded.is_secret, updated_by = excluded.updated_by, updated_at = now()",
        )
        .bind(module)
        .bind(k)
        .bind(&stored)
        .bind(secret)
        .bind(by)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    let keys: Vec<&String> = staged.iter().map(|(k, _, _)| k).collect();
    s.events
        .emit(
            "module.env_updated",
            None,
            json!({ "module": module, "keys": keys, "by": by }),
        )
        .await;
    describe(s, module).await
}

/// Remove one key (`Some`) or every binding of the module (`None`); returns how many rows went.
pub async fn unbind(s: &AppState, module: &str, key: Option<&str>, by: &str) -> Result<u64> {
    let n = match key {
        Some(k) => sqlx::query("delete from musubi.module_env where module = $1 and key = $2")
            .bind(module)
            .bind(k)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?
            .rows_affected(),
        None => sqlx::query("delete from musubi.module_env where module = $1")
            .bind(module)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?
            .rows_affected(),
    };
    if n > 0 {
        s.events
            .emit(
                "module.env_updated",
                None,
                json!({ "module": module, "removed": key, "count": n, "by": by }),
            )
            .await;
    }
    Ok(n)
}

/// `env.get` / `env.all` host ops — `__module` is stamped by the runtime, not the guest.
pub async fn host_op(s: &AppState, req: &Value) -> Result<Value> {
    let module = req["__module"]
        .as_str()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| Error::InvalidArgument("env ops need a module context".into()))?;
    let values = resolve(s, module).await?;
    match req["op"].as_str() {
        Some("env.all") => Ok(json!({ "module": module, "values": values })),
        _ => {
            let key = req["key"]
                .as_str()
                .ok_or_else(|| Error::InvalidArgument("host request needs string `key`".into()))?;
            let value = values
                .get(key)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("env `{module}/{key}`")))?;
            Ok(json!({ "module": module, "key": key, "value": value }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_scalars() {
        assert_eq!(coerce("int", json!("42")).unwrap(), json!(42));
        assert_eq!(coerce("int", json!(7.0)).unwrap(), json!(7));
        assert!(coerce("int", json!("4.5")).is_err());
        assert!(coerce("int", json!(true)).is_err());
        assert_eq!(coerce("float", json!("0.25")).unwrap(), json!(0.25));
        assert_eq!(coerce("bool", json!("Yes")).unwrap(), json!(true));
        assert_eq!(coerce("bool", json!(0)).unwrap(), json!(false));
        assert!(coerce("bool", json!("maybe")).is_err());
        assert_eq!(coerce("string", json!(12)).unwrap(), json!("12"));
        assert!(coerce("string", json!({"a": 1})).is_err());
        assert_eq!(coerce("json", json!({"a": 1})).unwrap(), json!({"a": 1}));
        assert!(coerce("blob", json!(1)).is_err());
    }

    #[test]
    fn key_rules() {
        assert!(valid_key("STRIPE_KEY").is_ok());
        assert!(valid_key("_x1").is_ok());
        assert!(valid_key("1abc").is_err());
        assert!(valid_key("a-b").is_err());
        assert!(valid_key("").is_err());
        assert!(valid_key(&"k".repeat(65)).is_err());
    }

    #[test]
    fn lint_declarations() {
        let idl = musubi_idl::Idl::parse(
            r#"
[env."shop/STRIPE_KEY"]
type = "string"
required = true
secret = true

[env."shop/MAX_CART"]
type = "int"
default = "ten"

[env."shop/MODE"]
type = "enum"

[env."nomodule"]
type = "string"
"#,
        )
        .unwrap();
        let problems = lint(&idl);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems
            .iter()
            .any(|p| p.contains("MAX_CART") && p.contains("default")));
        assert!(problems.iter().any(|p| p.contains("unknown type `enum`")));
        assert!(problems.iter().any(|p| p.contains("nomodule")));
        assert!(idl.env["shop/STRIPE_KEY"].secret);
    }
}

#[cfg(test)]
mod audit_tests {
    use super::coerce;
    use serde_json::json;

    #[test]
    fn int_coercion_rejects_floats_beyond_exact_range() {
        assert_eq!(coerce("int", json!(42.0)).unwrap(), json!(42));
        assert_eq!(coerce("int", json!(-3.0)).unwrap(), json!(-3));
        assert!(
            coerce("int", json!(1e300)).is_err(),
            "would saturate under `as i64`"
        );
        assert!(coerce("int", json!(-1e19)).is_err());
        assert!(coerce("int", json!(1.5)).is_err());
    }
}
