//! Matchmaker query language — the practical subset of Nakama/Bleve syntax clients already use:
//!
//! ```text
//! +properties.region:europe +properties.rank:>=5 +properties.rank:<=15 -properties.mode:ranked properties.lang:en
//! ```
//!
//! `+` terms are required, `-` terms prohibited, bare terms optional (counted as a preference, never
//! exclude). Values: exact string, numeric comparisons `>= > <= <`, inclusive ranges `[a TO b]` and `*`.

use std::collections::HashMap;

use musubi_core::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Any,
    Str(String),
    Num(f64),
    Cmp(Cmp, f64),
    Range(f64, f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Ge,
    Gt,
    Le,
    Lt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Must,
    MustNot,
    Should,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub mode: Mode,
    pub field: String,
    pub value: Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<Term>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Properties {
    pub strings: HashMap<String, String>,
    pub numbers: HashMap<String, f64>,
}

impl Query {
    pub fn parse(src: &str) -> Result<Query> {
        let mut terms = Vec::new();
        let mut rest = src.trim();
        while !rest.is_empty() {
            let (tok, tail) = next_token(rest)?;
            rest = tail.trim_start();
            if tok == "*" {
                continue;
            }
            let (mode, body) = match tok.chars().next() {
                Some('+') => (Mode::Must, &tok[1..]),
                Some('-') => (Mode::MustNot, &tok[1..]),
                _ => (Mode::Should, tok),
            };
            let (field, raw) = body.split_once(':').ok_or_else(|| {
                Error::InvalidArgument(format!("matchmaker query: term `{tok}` needs field:value"))
            })?;
            let field = field.strip_prefix("properties.").unwrap_or(field);
            if field.is_empty() {
                return Err(Error::InvalidArgument(
                    "matchmaker query: empty field".into(),
                ));
            }
            terms.push(Term {
                mode,
                field: field.to_string(),
                value: parse_value(raw)?,
            });
        }
        Ok(Query { terms })
    }

    /// Does `p` satisfy every `+` term and no `-` term?
    pub fn matches(&self, p: &Properties) -> bool {
        self.terms.iter().all(|t| match t.mode {
            Mode::Must => t.value.test(&t.field, p),
            Mode::MustNot => !t.value.test(&t.field, p),
            Mode::Should => true,
        })
    }

    /// Number of satisfied optional terms — a tie-breaker for ordering candidates.
    pub fn preference(&self, p: &Properties) -> usize {
        self.terms
            .iter()
            .filter(|t| t.mode == Mode::Should && t.value.test(&t.field, p))
            .count()
    }
}

impl Value {
    fn test(&self, field: &str, p: &Properties) -> bool {
        match self {
            Value::Any => p.strings.contains_key(field) || p.numbers.contains_key(field),
            Value::Str(s) => p.strings.get(field).is_some_and(|v| v == s),
            Value::Num(n) => {
                p.numbers.get(field).is_some_and(|v| v == n)
                    || p.strings
                        .get(field)
                        .is_some_and(|v| v.parse::<f64>().ok() == Some(*n))
            }
            Value::Cmp(c, n) => p.numbers.get(field).is_some_and(|v| match c {
                Cmp::Ge => v >= n,
                Cmp::Gt => v > n,
                Cmp::Le => v <= n,
                Cmp::Lt => v < n,
            }),
            Value::Range(lo, hi) => p.numbers.get(field).is_some_and(|v| v >= lo && v <= hi),
        }
    }
}

fn next_token(s: &str) -> Result<(&str, &str)> {
    // A token runs to the next whitespace, except inside `[... TO ...]` or quotes.
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut depth = 0;
    let mut quoted = false;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => quoted = !quoted,
            b'[' => depth += 1,
            b']' => depth -= 1,
            c if c.is_ascii_whitespace() && depth == 0 && !quoted => break,
            _ => {}
        }
        i += 1;
    }
    if quoted || depth != 0 {
        return Err(Error::InvalidArgument(
            "matchmaker query: unbalanced quotes or brackets".into(),
        ));
    }
    Ok((&s[..i], &s[i..]))
}

fn parse_value(raw: &str) -> Result<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::InvalidArgument(
            "matchmaker query: empty value".into(),
        ));
    }
    if raw == "*" {
        return Ok(Value::Any);
    }
    if let Some(inner) = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let (a, b) = inner.split_once(" TO ").ok_or_else(|| {
            Error::InvalidArgument(format!("matchmaker query: bad range `{raw}`"))
        })?;
        let lo = if a.trim() == "*" {
            f64::NEG_INFINITY
        } else {
            num(a)?
        };
        let hi = if b.trim() == "*" {
            f64::INFINITY
        } else {
            num(b)?
        };
        return Ok(Value::Range(lo, hi));
    }
    for (prefix, cmp) in [
        (">=", Cmp::Ge),
        ("<=", Cmp::Le),
        (">", Cmp::Gt),
        ("<", Cmp::Lt),
    ] {
        if let Some(n) = raw.strip_prefix(prefix) {
            return Ok(Value::Cmp(cmp, num(n)?));
        }
    }
    if let Some(q) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return Ok(Value::Str(q.to_string()));
    }
    if let Ok(n) = raw.parse::<f64>() {
        return Ok(Value::Num(n));
    }
    Ok(Value::Str(raw.to_string()))
}

fn num(s: &str) -> Result<f64> {
    s.trim()
        .parse::<f64>()
        .map_err(|_| Error::InvalidArgument(format!("matchmaker query: `{s}` is not a number")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(s: &[(&str, &str)], n: &[(&str, f64)]) -> Properties {
        Properties {
            strings: s
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            numbers: n.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn parses_and_matches() {
        let q = Query::parse(
            "+properties.region:europe +properties.rank:>=5 +rank:<=15 -mode:ranked lang:en",
        )
        .unwrap();
        assert_eq!(q.terms.len(), 5);
        let good = props(
            &[("region", "europe"), ("mode", "casual"), ("lang", "en")],
            &[("rank", 10.0)],
        );
        assert!(q.matches(&good));
        assert_eq!(q.preference(&good), 1);
        assert!(!q.matches(&props(&[("region", "asia")], &[("rank", 10.0)])));
        assert!(!q.matches(&props(&[("region", "europe")], &[("rank", 20.0)])));
        assert!(!q.matches(&props(
            &[("region", "europe"), ("mode", "ranked")],
            &[("rank", 10.0)]
        )));
        assert!(
            !q.matches(&props(&[("region", "europe")], &[])),
            "missing numeric field fails"
        );
    }

    #[test]
    fn ranges_wildcards_quotes() {
        let q = Query::parse(r#"+skill:[10 TO 20] +map:"de dust" +party:*"#).unwrap();
        assert!(q.matches(&props(
            &[("map", "de dust"), ("party", "x")],
            &[("skill", 15.0)]
        )));
        assert!(!q.matches(&props(
            &[("map", "de dust"), ("party", "x")],
            &[("skill", 25.0)]
        )));
        assert!(Query::parse("*").unwrap().matches(&props(&[], &[])));
        assert!(Query::parse("+rank:[* TO 5]")
            .unwrap()
            .matches(&props(&[], &[("rank", -3.0)])));
        assert!(Query::parse("+bad").is_err());
        assert!(Query::parse("+x:[1 TO").is_err());
    }
}
