//! Protocol model lifted from the Rust sources of `musubi-proto` with `syn`. Only what the
//! C++ generator needs: structs, enums (externally tagged, snake_case), field types and the
//! serde attributes that change the wire (`json_in_binary`, `pos_cm`, `default`).

use anyhow::{bail, Context, Result};
use syn::{Attribute, Fields, GenericArgument, Item, PathArguments, Type};

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Bool,
    U16,
    U32,
    U64,
    I32,
    I64,
    F64,
    /// `usize` on the wire is a postcard varint like `u64`.
    Usize,
    Str,
    Bytes,
    /// `AccountId`, `SessionId`, `Uuid`: JSON string, 16 raw bytes in postcard.
    Uuid,
    /// `serde_json::Value` / `JsonText`: raw JSON in JSON, JSON text as string in postcard.
    Json,
    Opt(Box<Ty>),
    Vec(Box<Ty>),
    Map(Box<Ty>),
    Named(String),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    /// `#[serde(with = "crate::pos_cm")]`: f64 in JSON, i32 centimetres in postcard.
    pub pos_cm: bool,
    /// `#[serde(default)]`: may be absent in JSON input.
    pub default: bool,
    /// `#[serde(rename = "...")]`: JSON key differs from the Rust field name.
    pub rename: Option<String>,
    /// `#[serde(default = "fn")]`: the function that supplies the default.
    pub default_fn: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub enum VariantKind {
    Unit,
    /// `Foo(Bar)`: externally tagged `{"foo": <Bar>}`.
    Newtype(Ty),
    Struct(Vec<Field>),
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub tag: String,
    pub kind: VariantKind,
}

#[derive(Debug, Clone)]
pub struct Enum {
    pub name: String,
    pub variants: Vec<Variant>,
    /// `#[serde(into = "i16", try_from = "i16")]` + explicit discriminants: an integer on the wire.
    pub int_repr: Option<Vec<(String, i64)>>,
}

#[derive(Debug, Default)]
pub struct Model {
    pub structs: Vec<Struct>,
    pub enums: Vec<Enum>,
}

pub fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn meta_string(m: &syn::Meta) -> String {
    use quote::ToTokens;
    m.to_token_stream().to_string().replace(' ', "")
}

/// Value of `#[serde(key = "value")]`, if present.
fn serde_attr_value(attrs: &[Attribute], key: &str) -> Option<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("serde"))
        .find_map(|a| {
            let m = meta_string(&a.meta);
            let pat = format!("{key}=\"");
            let start = m.find(&pat)? + pat.len();
            let end = m[start..].find('"')? + start;
            Some(m[start..end].to_string())
        })
}

fn serde_attr_has(attrs: &[Attribute], needle: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("serde") && meta_string(&a.meta).contains(needle))
}

pub fn parse_ty(t: &Type) -> Result<Ty> {
    let Type::Path(p) = t else {
        bail!("unsupported type {:?}", t)
    };
    let seg = p.path.segments.last().context("empty path")?;
    let ident = seg.ident.to_string();
    let arg = |n: usize| -> Result<Ty> {
        let PathArguments::AngleBracketed(a) = &seg.arguments else {
            bail!("{ident}: missing generics")
        };
        match a.args.iter().nth(n) {
            Some(GenericArgument::Type(t)) => parse_ty(t),
            _ => bail!("{ident}: bad generic arg"),
        }
    };
    Ok(match ident.as_str() {
        "bool" => Ty::Bool,
        "u16" => Ty::U16,
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        "i32" => Ty::I32,
        "i64" => Ty::I64,
        "f64" => Ty::F64,
        "usize" => Ty::Usize,
        "String" => Ty::Str,
        "Arc" => {
            let PathArguments::AngleBracketed(a) = &seg.arguments else {
                bail!("Arc")
            };
            match a.args.first() {
                Some(GenericArgument::Type(Type::Path(ip))) if ip.path.is_ident("str") => Ty::Str,
                _ => bail!("unsupported Arc<..>"),
            }
        }
        "Uuid" | "AccountId" | "SessionId" => Ty::Uuid,
        "Value" | "JsonText" => Ty::Json,
        "Vec" => {
            let inner = arg(0)?;
            if inner == Ty::U16 || inner == Ty::U32 {
                Ty::Vec(Box::new(inner))
            } else if matches!(inner, Ty::Named(ref n) if n == "u8") {
                Ty::Bytes
            } else {
                Ty::Vec(Box::new(inner))
            }
        }
        "Option" => Ty::Opt(Box::new(arg(0)?)),
        "HashMap" | "BTreeMap" => {
            let k = arg(0)?;
            if k != Ty::Str {
                bail!("map key must be String");
            }
            Ty::Map(Box::new(arg(1)?))
        }
        "u8" => Ty::Named("u8".into()),
        other => Ty::Named(other.to_string()),
    })
}

fn parse_fields(f: &Fields) -> Result<Vec<Field>> {
    let Fields::Named(named) = f else {
        bail!("expected named fields")
    };
    named
        .named
        .iter()
        .map(|f| {
            Ok(Field {
                name: f.ident.as_ref().context("unnamed")?.to_string(),
                ty: parse_ty(&f.ty)?,
                pos_cm: serde_attr_has(&f.attrs, "pos_cm"),
                default: serde_attr_has(&f.attrs, "default"),
                rename: serde_attr_value(&f.attrs, "rename"),
                default_fn: serde_attr_value(&f.attrs, "default"),
            })
        })
        .collect()
}

/// Lift the listed types out of a Rust source file.
pub fn lift(src: &str, wanted: &[&str], model: &mut Model) -> Result<()> {
    let file = syn::parse_file(src)?;
    for item in file.items {
        match item {
            Item::Struct(s) if wanted.contains(&s.ident.to_string().as_str()) => {
                model.structs.push(Struct {
                    name: s.ident.to_string(),
                    fields: parse_fields(&s.fields)?,
                });
            }
            Item::Enum(e) if wanted.contains(&e.ident.to_string().as_str()) => {
                let int_repr = if serde_attr_has(&e.attrs, "into=\"i16\"") {
                    let mut vals = Vec::new();
                    for v in &e.variants {
                        let Some((_, syn::Expr::Lit(l))) = &v.discriminant else {
                            bail!("{}: repr enum without discriminant", e.ident)
                        };
                        let syn::Lit::Int(i) = &l.lit else {
                            bail!("bad discriminant")
                        };
                        vals.push((v.ident.to_string(), i.base10_parse::<i64>()?));
                    }
                    Some(vals)
                } else {
                    None
                };
                let mut variants = Vec::new();
                for v in &e.variants {
                    let kind = match &v.fields {
                        Fields::Unit => VariantKind::Unit,
                        Fields::Unnamed(u) if u.unnamed.len() == 1 => {
                            VariantKind::Newtype(parse_ty(&u.unnamed[0].ty)?)
                        }
                        Fields::Unnamed(_) => bail!("tuple variants unsupported"),
                        Fields::Named(_) => VariantKind::Struct(parse_fields(&v.fields)?),
                    };
                    variants.push(Variant {
                        name: v.ident.to_string(),
                        tag: snake(&v.ident.to_string()),
                        kind,
                    });
                }
                model.enums.push(Enum {
                    name: e.ident.to_string(),
                    variants,
                    int_repr,
                });
            }
            _ => {}
        }
    }
    Ok(())
}
