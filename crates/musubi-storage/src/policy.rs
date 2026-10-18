//! Collection access policies (plan 3.5): a small CEL-like expression per collection that must
//! evaluate to `true` for a user caller to read / write / delete an object. Server callers are
//! never subject to policies. Variables: `me` (caller account id or null), `owner`, `key`,
//! `collection`, `op` (`"read"|"write"|"delete"`), `permission` (the object's read or write
//! permission code), `now` (unix seconds), `value` (the stored object or null), `new` (the
//! incoming value on write, else null). Operators: `|| && ! == != < <= > >= in + - * /`,
//! unary `-`, dotted paths (`value.team.id`), lists `[1, 2]`, strings `'x'`/`"x"`, numbers,
//! `true`/`false`/`null`. Functions: `has(value.x)`, `size(x)`, `startsWith(s, p)`,
//! `endsWith(s, p)`, `contains(x, y)` (string or list), `matches(s, regex)`, `int(x)`,
//! `string(x)`. Missing fields are `null`; anything but a `true` result denies.

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Per-collection expressions; an absent expression keeps the default permission rules.
/// `delete` falls back to `write` when absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Policy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
}

impl Policy {
    pub fn is_empty(&self) -> bool {
        self.read.is_none() && self.write.is_none() && self.delete.is_none()
    }

    /// Parse every expression; errors name the offending rule.
    pub fn validate(&self) -> Result<()> {
        for (what, e) in [
            ("read", &self.read),
            ("write", &self.write),
            ("delete", &self.delete),
        ] {
            if let Some(src) = e {
                if src.len() > 4096 {
                    return Err(Error::InvalidArgument(format!(
                        "policy.{what}: at most 4096 chars"
                    )));
                }
                parse(src).map_err(|m| Error::InvalidArgument(format!("policy.{what}: {m}")))?;
            }
        }
        Ok(())
    }

    pub fn delete_rule(&self) -> Option<&str> {
        self.delete.as_deref().or(self.write.as_deref())
    }
}

/// Everything an expression can look at.
pub struct Env<'a> {
    pub me: Option<uuid::Uuid>,
    pub owner: uuid::Uuid,
    pub key: &'a str,
    pub collection: &'a str,
    pub op: &'a str,
    pub permission: i16,
    pub value: Option<&'a Value>,
    pub new: Option<&'a Value>,
}

/// `true` iff `src` evaluates to boolean true in `env`. Parse/eval errors deny.
pub fn allows(src: &str, env: &Env<'_>) -> bool {
    match parse(src) {
        Ok(e) => matches!(eval(&e, env), Ok(Value::Bool(true))),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------- lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
}

fn lex(src: &str) -> std::result::Result<Vec<Tok>, String> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
        let op2 = ["||", "&&", "==", "!=", "<=", ">="]
            .iter()
            .find(|o| **o == two)
            .copied();
        if let Some(o) = op2 {
            out.push(Tok::Op(o));
            i += 2;
            continue;
        }
        match c {
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            '[' => out.push(Tok::LBracket),
            ']' => out.push(Tok::RBracket),
            ',' => out.push(Tok::Comma),
            '.' if !cs.get(i + 1).is_some_and(|d| d.is_ascii_digit()) => out.push(Tok::Dot),
            '!' => out.push(Tok::Op("!")),
            '<' => out.push(Tok::Op("<")),
            '>' => out.push(Tok::Op(">")),
            '+' => out.push(Tok::Op("+")),
            '-' => out.push(Tok::Op("-")),
            '*' => out.push(Tok::Op("*")),
            '/' => out.push(Tok::Op("/")),
            '\'' | '"' => {
                let q = c;
                let mut s = String::new();
                i += 1;
                loop {
                    let Some(&d) = cs.get(i) else {
                        return Err("unterminated string".into());
                    };
                    i += 1;
                    if d == q {
                        break;
                    }
                    if d == '\\' {
                        let Some(&e) = cs.get(i) else {
                            return Err("bad escape".into());
                        };
                        i += 1;
                        s.push(match e {
                            'n' => '\n',
                            't' => '\t',
                            o => o,
                        });
                    } else {
                        s.push(d);
                    }
                }
                out.push(Tok::Str(s));
                continue;
            }
            d if d.is_ascii_digit() || d == '.' => {
                let st = i;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                    i += 1;
                }
                let t: String = cs[st..i].iter().collect();
                out.push(Tok::Num(t.parse().map_err(|_| format!("bad number {t}"))?));
                continue;
            }
            d if d.is_alphabetic() || d == '_' => {
                let st = i;
                while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                    i += 1;
                }
                let t: String = cs[st..i].iter().collect();
                if t == "in" {
                    out.push(Tok::Op("in"));
                } else {
                    out.push(Tok::Ident(t));
                }
                continue;
            }
            o => return Err(format!("unexpected character {o:?}")),
        }
        i += 1;
    }
    Ok(out)
}

// ---------------------------------------------------------------- parser

#[derive(Debug, Clone)]
enum Expr {
    Lit(Value),
    Path(Vec<String>),
    List(Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

fn parse(src: &str) -> std::result::Result<Expr, String> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err("empty expression".into());
    }
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let e = p.expr(0)?;
    if p.pos != p.toks.len() {
        return Err(format!("unexpected token {:?}", p.toks[p.pos]));
    }
    Ok(e)
}

fn prec(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" => 3,
        "+" | "-" => 4,
        "*" | "/" => 5,
        _ => return None,
    })
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, t: Tok) -> std::result::Result<(), String> {
        match self.next() {
            Some(x) if x == t => Ok(()),
            other => Err(format!("expected {t:?}, got {other:?}")),
        }
    }

    fn expr(&mut self, min: u8) -> std::result::Result<Expr, String> {
        self.depth += 1;
        if self.depth > 64 {
            return Err("expression too deep".into());
        }
        let mut lhs = self.unary()?;
        while let Some(Tok::Op(op)) = self.peek() {
            let op = *op;
            let Some(p) = prec(op) else { break };
            if p < min {
                break;
            }
            self.pos += 1;
            let rhs = self.expr(p + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        self.depth -= 1;
        Ok(lhs)
    }

    fn unary(&mut self) -> std::result::Result<Expr, String> {
        match self.peek() {
            Some(Tok::Op("!")) => {
                self.pos += 1;
                Ok(Expr::Unary("!", Box::new(self.unary()?)))
            }
            Some(Tok::Op("-")) => {
                self.pos += 1;
                Ok(Expr::Unary("-", Box::new(self.unary()?)))
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> std::result::Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Lit(serde_json::json!(n))),
            Some(Tok::Str(s)) => Ok(Expr::Lit(Value::String(s))),
            Some(Tok::LParen) => {
                let e = self.expr(0)?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::LBracket) => {
                let mut items = Vec::new();
                if self.peek() == Some(&Tok::RBracket) {
                    self.pos += 1;
                    return Ok(Expr::List(items));
                }
                loop {
                    items.push(self.expr(0)?);
                    match self.next() {
                        Some(Tok::Comma) => continue,
                        Some(Tok::RBracket) => break,
                        other => return Err(format!("expected , or ] got {other:?}")),
                    }
                }
                Ok(Expr::List(items))
            }
            Some(Tok::Ident(id)) => {
                match id.as_str() {
                    "true" => return Ok(Expr::Lit(Value::Bool(true))),
                    "false" => return Ok(Expr::Lit(Value::Bool(false))),
                    "null" => return Ok(Expr::Lit(Value::Null)),
                    _ => {}
                }
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::RParen) {
                        self.pos += 1;
                    } else {
                        loop {
                            args.push(self.expr(0)?);
                            match self.next() {
                                Some(Tok::Comma) => continue,
                                Some(Tok::RParen) => break,
                                other => return Err(format!("expected , or ) got {other:?}")),
                            }
                        }
                    }
                    if !matches!(
                        id.as_str(),
                        "has"
                            | "size"
                            | "startsWith"
                            | "endsWith"
                            | "contains"
                            | "matches"
                            | "int"
                            | "string"
                    ) {
                        return Err(format!("unknown function {id}"));
                    }
                    return Ok(Expr::Call(id, args));
                }
                let mut path = vec![id];
                while self.peek() == Some(&Tok::Dot) {
                    self.pos += 1;
                    match self.next() {
                        Some(Tok::Ident(seg)) => path.push(seg),
                        Some(Tok::Op("in")) => path.push("in".into()),
                        other => return Err(format!("expected field after '.', got {other:?}")),
                    }
                }
                if !matches!(
                    path[0].as_str(),
                    "me" | "owner"
                        | "key"
                        | "collection"
                        | "op"
                        | "permission"
                        | "now"
                        | "value"
                        | "new"
                ) {
                    return Err(format!("unknown variable {}", path[0]));
                }
                Ok(Expr::Path(path))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

// ---------------------------------------------------------------- eval

fn lookup(path: &[String], env: &Env<'_>) -> Value {
    let root = match path[0].as_str() {
        "me" => env
            .me
            .map(|u| Value::String(u.to_string()))
            .unwrap_or(Value::Null),
        "owner" => Value::String(env.owner.to_string()),
        "key" => Value::String(env.key.to_string()),
        "collection" => Value::String(env.collection.to_string()),
        "op" => Value::String(env.op.to_string()),
        "permission" => serde_json::json!(env.permission),
        "now" => serde_json::json!(time::OffsetDateTime::now_utc().unix_timestamp()),
        "value" => env.value.cloned().unwrap_or(Value::Null),
        "new" => env.new.cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let mut cur = root;
    for seg in &path[1..] {
        cur = match cur {
            Value::Object(mut m) => m.remove(seg).unwrap_or(Value::Null),
            Value::Array(a) => seg
                .parse::<usize>()
                .ok()
                .and_then(|i| a.get(i).cloned())
                .unwrap_or(Value::Null),
            _ => Value::Null,
        };
    }
    cur
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// Equality where `2 == 2.0` (literals parse as floats, JSON data is often integral).
fn veq(a: &Value, b: &Value) -> bool {
    match (num(a), num(b)) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => x == y,
        _ => a == b,
    }
}

fn cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => num(a)?.partial_cmp(&num(b)?),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn eval(e: &Expr, env: &Env<'_>) -> std::result::Result<Value, String> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Path(p) => lookup(p, env),
        Expr::List(items) => Value::Array(
            items
                .iter()
                .map(|i| eval(i, env))
                .collect::<std::result::Result<_, _>>()?,
        ),
        Expr::Unary("!", x) => match eval(x, env)? {
            Value::Bool(b) => Value::Bool(!b),
            Value::Null => Value::Bool(true),
            _ => return Err("! needs a bool".into()),
        },
        Expr::Unary(_, x) => match num(&eval(x, env)?) {
            Some(n) => serde_json::json!(-n),
            None => return Err("- needs a number".into()),
        },
        Expr::Binary("||", a, b) => {
            if eval(a, env)? == Value::Bool(true) {
                Value::Bool(true)
            } else {
                Value::Bool(eval(b, env)? == Value::Bool(true))
            }
        }
        Expr::Binary("&&", a, b) => {
            if eval(a, env)? != Value::Bool(true) {
                Value::Bool(false)
            } else {
                Value::Bool(eval(b, env)? == Value::Bool(true))
            }
        }
        Expr::Binary(op, a, b) => {
            let (x, y) = (eval(a, env)?, eval(b, env)?);
            match *op {
                "==" => Value::Bool(veq(&x, &y)),
                "!=" => Value::Bool(!veq(&x, &y)),
                "in" => Value::Bool(match &y {
                    Value::Array(items) => items.iter().any(|i| veq(i, &x)),
                    Value::Object(m) => x.as_str().is_some_and(|k| m.contains_key(k)),
                    Value::String(s) => x.as_str().is_some_and(|k| s.contains(k)),
                    _ => false,
                }),
                "<" | "<=" | ">" | ">=" => {
                    let Some(o) = cmp(&x, &y) else {
                        return Ok(Value::Bool(false));
                    };
                    Value::Bool(match *op {
                        "<" => o.is_lt(),
                        "<=" => o.is_le(),
                        ">" => o.is_gt(),
                        _ => o.is_ge(),
                    })
                }
                "+" => match (&x, &y) {
                    (Value::String(p), Value::String(q)) => Value::String(format!("{p}{q}")),
                    _ => match (num(&x), num(&y)) {
                        (Some(p), Some(q)) => serde_json::json!(p + q),
                        _ => return Err("+ needs numbers or strings".into()),
                    },
                },
                "-" | "*" | "/" => match (num(&x), num(&y)) {
                    (Some(p), Some(q)) => serde_json::json!(match *op {
                        "-" => p - q,
                        "*" => p * q,
                        _ => {
                            if q == 0.0 {
                                return Err("division by zero".into());
                            }
                            p / q
                        }
                    }),
                    _ => return Err(format!("{op} needs numbers")),
                },
                _ => return Err(format!("unknown operator {op}")),
            }
        }
        Expr::Call(f, args) => call(f, args, env)?,
    })
}

fn call(f: &str, args: &[Expr], env: &Env<'_>) -> std::result::Result<Value, String> {
    let arity = |n: usize| {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("{f} takes {n} argument(s)"))
        }
    };
    Ok(match f {
        "has" => {
            arity(1)?;
            let Expr::Path(p) = &args[0] else {
                return Err("has() needs a field path".into());
            };
            Value::Bool(lookup(p, env) != Value::Null)
        }
        "size" => {
            arity(1)?;
            match eval(&args[0], env)? {
                Value::String(s) => serde_json::json!(s.chars().count()),
                Value::Array(a) => serde_json::json!(a.len()),
                Value::Object(m) => serde_json::json!(m.len()),
                Value::Null => serde_json::json!(0),
                _ => return Err("size() needs a string, list or object".into()),
            }
        }
        "startsWith" | "endsWith" => {
            arity(2)?;
            let (s, p) = (eval(&args[0], env)?, eval(&args[1], env)?);
            match (s.as_str(), p.as_str()) {
                (Some(s), Some(p)) => Value::Bool(if f == "startsWith" {
                    s.starts_with(p)
                } else {
                    s.ends_with(p)
                }),
                _ => Value::Bool(false),
            }
        }
        "contains" => {
            arity(2)?;
            let (x, y) = (eval(&args[0], env)?, eval(&args[1], env)?);
            Value::Bool(match &x {
                Value::String(s) => y.as_str().is_some_and(|n| s.contains(n)),
                Value::Array(a) => a.iter().any(|i| veq(i, &y)),
                Value::Object(m) => y.as_str().is_some_and(|k| m.contains_key(k)),
                _ => false,
            })
        }
        "matches" => {
            arity(2)?;
            let (s, re) = (eval(&args[0], env)?, eval(&args[1], env)?);
            match (s.as_str(), re.as_str()) {
                (Some(s), Some(re)) => {
                    if re.len() > 256 {
                        return Err("regex too long".into());
                    }
                    let re = regex::RegexBuilder::new(re)
                        .size_limit(1 << 16)
                        .build()
                        .map_err(|e| format!("matches(): {e}"))?;
                    Value::Bool(re.is_match(s))
                }
                _ => Value::Bool(false),
            }
        }
        "int" => {
            arity(1)?;
            match eval(&args[0], env)? {
                Value::Number(n) => serde_json::json!(n.as_f64().unwrap_or(0.0).trunc()),
                Value::String(s) => match s.trim().parse::<f64>() {
                    Ok(n) => serde_json::json!(n.trunc()),
                    Err(_) => Value::Null,
                },
                Value::Bool(b) => serde_json::json!(if b { 1 } else { 0 }),
                _ => Value::Null,
            }
        }
        "string" => {
            arity(1)?;
            match eval(&args[0], env)? {
                Value::String(s) => Value::String(s),
                Value::Null => Value::Null,
                Value::Number(n) => Value::String(match n.as_f64() {
                    Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
                    _ => n.to_string(),
                }),
                v => Value::String(v.to_string()),
            }
        }
        _ => return Err(format!("unknown function {f}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env<'a>(
        me: Option<uuid::Uuid>,
        owner: uuid::Uuid,
        value: &'a Value,
        new: Option<&'a Value>,
    ) -> Env<'a> {
        Env {
            me,
            owner,
            key: "k1",
            collection: "profiles",
            op: "read",
            permission: 1,
            value: Some(value),
            new,
        }
    }

    #[test]
    fn expressions() {
        let me = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        let v = json!({"public": true, "team": {"id": "red", "members": ["a", "b"]}, "level": 7, "name": "Zed"});
        let e = env(Some(me), me, &v, None);
        for (src, want) in [
            ("owner == me", true),
            ("owner != me", false),
            ("value.public == true || owner == me", true),
            ("value.team.id in ['red', 'blue']", true),
            ("'a' in value.team.members", true),
            ("size(value.team.members) == 2", true),
            ("value.level >= 5 && value.level < 10", true),
            ("has(value.team.id) && !has(value.missing)", true),
            ("value.missing == null", true),
            ("startsWith(value.name, 'Z') && endsWith(key, '1')", true),
            ("contains(value.name, 'ed')", true),
            ("matches(value.name, '^[A-Z][a-z]+$')", true),
            (
                "collection == 'profiles' && op == 'read' && permission == 1",
                true,
            ),
            ("now > 1700000000", true),
            ("value.level * 2 - 4 == 10", true),
            ("int('42') == 42 && string(7) == '7'", true),
            ("value.level", false),
            ("'x'", false),
            ("value.level > 'a'", false),
            ("-value.level == -7", true),
        ] {
            assert_eq!(allows(src, &e), want, "{src}");
        }
        let e2 = env(Some(other), me, &v, None);
        assert!(!allows("owner == me", &e2));
        assert!(allows("owner == me || value.public == true", &e2));
        let anon = env(None, me, &v, None);
        assert!(allows("me == null", &anon));
        let nv = json!({"public": false, "locked": true});
        let e3 = env(Some(me), me, &v, Some(&nv));
        assert!(allows("new.locked == true && value.public == true", &e3));
        assert!(!allows("new.public == true", &e3));
    }

    #[test]
    fn parse_errors_and_validation() {
        for src in [
            "",
            "foo == 1",
            "value.level >",
            "unknownfn(1)",
            "(value.level",
            "'unterminated",
            "value.level ==== 1",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        let p = Policy {
            read: Some("owner == me".into()),
            write: Some("size(new.name) <= 20".into()),
            delete: None,
        };
        p.validate().unwrap();
        assert_eq!(p.delete_rule(), Some("size(new.name) <= 20"));
        let bad = Policy {
            read: Some("owner ==".into()),
            ..Default::default()
        };
        assert!(bad
            .validate()
            .unwrap_err()
            .to_string()
            .contains("policy.read"));
    }

    #[test]
    fn eval_errors_deny() {
        let me = uuid::Uuid::now_v7();
        let v = json!({"n": 1});
        let e = env(Some(me), me, &v, None);
        assert!(!allows("value.n / 0 == 1", &e));
        assert!(!allows("!value.n", &e));
        assert!(!allows("matches(key, '(')", &e));
    }
}
