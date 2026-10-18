//! Typed match state sync (plan 5.8 v2): when a match module declares `state` in the IDL with
//! `sync = true`, the node keeps the last JSON state it sent each player and, after every
//! handler call, broadcasts the RFC 6902 patch from the previous to the new state
//! (`match_state_delta`), a full snapshot on join (`match_state`). `sync_exclude` paths
//! (JSON pointers) are server-private and stripped before diffing, so secrets never leave the
//! node even by accident.

use serde_json::{json, Map, Value};

/// RFC 6902 operations turning `prev` into `next`. Objects diff per key, arrays per index
/// (tail elements are added/removed), scalars become `replace`.
pub fn diff(prev: &Value, next: &Value) -> Vec<Value> {
    let mut ops = Vec::new();
    diff_into(prev, next, &mut String::new(), &mut ops);
    ops
}

fn escape(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

fn diff_into(prev: &Value, next: &Value, path: &mut String, ops: &mut Vec<Value>) {
    match (prev, next) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, va) in a {
                let len = path.len();
                path.push('/');
                path.push_str(&escape(k));
                match b.get(k) {
                    Some(vb) => diff_into(va, vb, path, ops),
                    None => ops.push(json!({ "op": "remove", "path": path })),
                }
                path.truncate(len);
            }
            for (k, vb) in b {
                if !a.contains_key(k) {
                    let len = path.len();
                    path.push('/');
                    path.push_str(&escape(k));
                    ops.push(json!({ "op": "add", "path": path, "value": vb }));
                    path.truncate(len);
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            let common = a.len().min(b.len());
            for i in 0..common {
                let len = path.len();
                path.push('/');
                path.push_str(&i.to_string());
                diff_into(&a[i], &b[i], path, ops);
                path.truncate(len);
            }
            // remove from the end so indices stay valid while applying in order
            for i in (common..a.len()).rev() {
                ops.push(json!({ "op": "remove", "path": format!("{path}/{i}") }));
            }
            for item in &b[common..] {
                ops.push(json!({ "op": "add", "path": format!("{path}/-"), "value": item }));
            }
        }
        (a, b) => {
            if a != b {
                ops.push(json!({ "op": "replace", "path": path, "value": b }));
            }
        }
    }
}

/// Remove the server-private paths (JSON pointers, `/secret`, `/players/*/hand` with `*` as
/// a wildcard for one object key or array index) from a copy of `state`.
pub fn strip(state: &Value, exclude: &[String]) -> Value {
    let mut out = state.clone();
    for ptr in exclude {
        let segs: Vec<&str> = ptr
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        if !segs.is_empty() {
            strip_at(&mut out, &segs);
        }
    }
    out
}

fn strip_at(v: &mut Value, segs: &[&str]) {
    let (head, rest) = (segs[0].replace("~1", "/").replace("~0", "~"), &segs[1..]);
    match v {
        Value::Object(m) => {
            if head == "*" {
                for child in m.values_mut() {
                    if rest.is_empty() {
                        *child = Value::Null;
                    } else {
                        strip_at(child, rest);
                    }
                }
                if rest.is_empty() {
                    m.clear();
                }
            } else if rest.is_empty() {
                m.remove(&head);
            } else if let Some(child) = m.get_mut(&head) {
                strip_at(child, rest);
            }
        }
        Value::Array(a) => {
            if head == "*" {
                if rest.is_empty() {
                    a.clear();
                } else {
                    for child in a.iter_mut() {
                        strip_at(child, rest);
                    }
                }
            } else if let Ok(i) = head.parse::<usize>() {
                if rest.is_empty() {
                    if i < a.len() {
                        a.remove(i);
                    }
                } else if let Some(child) = a.get_mut(i) {
                    strip_at(child, rest);
                }
            }
        }
        _ => {}
    }
}

/// Per-match sync bookkeeping: the last state each connected player has seen.
#[derive(Default)]
pub struct SyncState {
    pub exclude: Vec<String>,
    pub last: Option<Value>,
}

impl SyncState {
    pub fn new(exclude: Vec<String>) -> Self {
        Self {
            exclude,
            last: None,
        }
    }

    /// Public view of `state` (private paths stripped).
    pub fn view(&self, state: &Value) -> Value {
        if self.exclude.is_empty() {
            state.clone()
        } else {
            strip(state, &self.exclude)
        }
    }

    /// Patch from the last broadcast view to `state`'s view; remembers the new view. `None`
    /// when nothing visible changed. The first call yields a full `replace` of the root.
    pub fn advance(&mut self, state: &Value) -> Option<Vec<Value>> {
        let next = self.view(state);
        let ops = match &self.last {
            Some(prev) => diff(prev, &next),
            None => vec![json!({ "op": "replace", "path": "", "value": next })],
        };
        self.last = Some(next);
        (!ops.is_empty()).then_some(ops)
    }
}

/// Decode a handler's state as JSON when it is one (modules with binary state are not synced).
pub fn as_json(state: &[u8]) -> Option<Value> {
    if state.is_empty() {
        return Some(Value::Object(Map::new()));
    }
    serde_json::from_slice(state).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(prev: Value, next: Value) -> Vec<Value> {
        let ops = diff(&prev, &next);
        let mut doc = prev;
        musubi_storage::apply_json_patch(&mut doc, &ops).unwrap();
        assert_eq!(doc, next, "{ops:?}");
        ops
    }

    #[test]
    fn diff_objects_arrays_scalars() {
        let ops = roundtrip(
            json!({"round": 1, "players": {"a": {"hp": 10}, "b": {"hp": 5}}, "log": [1, 2, 3], "x": "y"}),
            json!({"round": 2, "players": {"a": {"hp": 7}, "c": {"hp": 9}}, "log": [1, 9], "z": true}),
        );
        assert_eq!(ops.len(), 8, "{ops:?}");
        assert!(ops.contains(&json!({"op": "replace", "path": "/round", "value": 2})));
        assert!(ops.contains(&json!({"op": "replace", "path": "/players/a/hp", "value": 7})));
        assert!(ops.contains(&json!({"op": "remove", "path": "/players/b"})));
        assert!(ops.contains(&json!({"op": "remove", "path": "/log/2"})));
        roundtrip(json!({"l": [1]}), json!({"l": [1, 2, 3]}));
        roundtrip(json!({"l": [1, 2, 3]}), json!({"l": []}));
        roundtrip(json!({"a/b": 1, "t~": 2}), json!({"a/b": 2}));
        roundtrip(json!([1, {"k": 1}]), json!({"k": 2}));
        assert!(diff(&json!({"a": [1, 2]}), &json!({"a": [1, 2]})).is_empty());
    }

    #[test]
    fn strip_private_paths() {
        let s = json!({"secret": 1, "players": {"a": {"hand": [1], "hp": 3}, "b": {"hand": [2], "hp": 4}}, "deck": [5, 6], "pub": true});
        let v = strip(
            &s,
            &[
                "/secret".into(),
                "/players/*/hand".into(),
                "/deck".into(),
                "/missing/x".into(),
            ],
        );
        assert_eq!(
            v,
            json!({"players": {"a": {"hp": 3}, "b": {"hp": 4}}, "pub": true})
        );
        assert_eq!(
            strip(&json!([{"a": 1}, {"a": 2}]), &["/*/a".into()]),
            json!([{}, {}])
        );
    }

    #[test]
    fn sync_state_first_full_then_deltas() {
        let mut sync = SyncState::new(vec!["/secret".into()]);
        let first = sync.advance(&json!({"round": 1, "secret": 9})).unwrap();
        assert_eq!(
            first,
            vec![json!({"op": "replace", "path": "", "value": {"round": 1}})]
        );
        assert!(
            sync.advance(&json!({"round": 1, "secret": 10})).is_none(),
            "private change is invisible"
        );
        let d = sync.advance(&json!({"round": 2, "secret": 10})).unwrap();
        assert_eq!(
            d,
            vec![json!({"op": "replace", "path": "/round", "value": 2})]
        );
        assert_eq!(as_json(b""), Some(json!({})));
        assert_eq!(as_json(b"\x00\x01"), None);
    }
}
