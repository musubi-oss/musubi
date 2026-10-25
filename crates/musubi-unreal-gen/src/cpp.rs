//! C++ emitter: one header with the structs/enums and one .cpp with JSON + postcard codecs.
//! Everything leans on three hand-written runtime headers in the plugin: `MusubiJson.h`
//! (`FMusubiJson`, `FJsonObject` helpers), `MusubiPostcard.h` (`FMusubiPostcardWriter/Reader`)
//! and `MusubiWireTypes.h` (`FMusubiUuid`).

use crate::model::{Enum, Field, Model, Struct, Ty, VariantKind};
use std::fmt::Write;

pub struct Out {
    pub header: String,
    pub cpp: String,
}

fn cpp_ty(model: &Model, t: &Ty) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::U8 => "uint8".into(),
        Ty::U16 => "uint16".into(),
        Ty::U32 => "uint32".into(),
        Ty::U64 | Ty::Usize => "uint64".into(),
        Ty::I32 => "int32".into(),
        Ty::I64 => "int64".into(),
        Ty::F64 => "double".into(),
        Ty::Str => "FString".into(),
        Ty::Bytes => "TArray<uint8>".into(),
        Ty::Uuid => "FMusubiUuid".into(),
        Ty::Json => "FMusubiJson".into(),
        Ty::Opt(i) => format!("TOptional<{}>", cpp_ty(model, i)),
        Ty::Vec(i) => format!("TArray<{}>", cpp_ty(model, i)),
        Ty::Map(v) => format!("TMap<FString, {}>", cpp_ty(model, v)),
        Ty::Named(n) => {
            if is_enum(model, n)
                .map(|e| e.int_repr.is_some())
                .unwrap_or(false)
            {
                format!("EMusubi{n}")
            } else {
                format!("FMusubi{n}")
            }
        }
    }
}

fn is_enum<'a>(model: &'a Model, n: &str) -> Option<&'a Enum> {
    model.enums.iter().find(|e| e.name == n)
}

/// Expression producing a `TSharedPtr<FJsonValue>` from `expr` of type `t`.
fn to_json(model: &Model, t: &Ty, expr: &str, pos_cm: bool) -> String {
    let _ = pos_cm;
    match t {
        Ty::Bool => format!("MakeShared<FJsonValueBoolean>({expr})"),
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::I32 | Ty::F64 => {
            format!("MakeShared<FJsonValueNumber>((double){expr})")
        }
        Ty::U64 | Ty::Usize | Ty::I64 => format!("MakeShared<FJsonValueNumber>((double){expr})"),
        Ty::Str => format!("MakeShared<FJsonValueString>({expr})"),
        Ty::Bytes => format!("MusubiJson::BytesToJson({expr})"),
        Ty::Uuid => format!("MakeShared<FJsonValueString>({expr}.ToString())"),
        Ty::Json => format!("MusubiJson::JsonOrNull({expr})"),
        Ty::Opt(i) => format!(
            "({expr}.IsSet() ? TSharedPtr<FJsonValue>({}) : MusubiJson::Null())",
            to_json(model, i, &format!("{expr}.GetValue()"), pos_cm)
        ),
        Ty::Vec(i) => format!(
            "MusubiJson::ArrayToJson<{}>({expr}, [](const {}& V) {{ return {}; }})",
            cpp_ty(model, i),
            cpp_ty(model, i),
            to_json(model, i, "V", pos_cm)
        ),
        Ty::Map(v) => format!(
            "MusubiJson::MapToJson<{}>({expr}, [](const {}& V) {{ return {}; }})",
            cpp_ty(model, v),
            cpp_ty(model, v),
            to_json(model, v, "V", pos_cm)
        ),
        Ty::Named(n) => {
            if is_enum(model, n)
                .map(|e| e.int_repr.is_some())
                .unwrap_or(false)
            {
                format!("MakeShared<FJsonValueNumber>((double)(int32){expr})")
            } else {
                format!("FMusubi{n}::ToJson({expr})")
            }
        }
    }
}

/// Statement(s) reading `TSharedPtr<FJsonValue> J` into `target` of type `t`; sets `bOk=false` on type errors.
fn from_json(model: &Model, t: &Ty, target: &str, j: &str, depth: usize) -> String {
    let v = format!("V{depth}");
    match t {
        Ty::Bool => format!("{{ bool {v}; if ({j}->TryGetBool({v})) {target} = {v}; else bOk = false; }}"),
        Ty::U8 => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = (uint8){v}; else bOk = false; }}"),
        Ty::U16 => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = (uint16){v}; else bOk = false; }}"),
        Ty::U32 => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = (uint32){v}; else bOk = false; }}"),
        Ty::I32 => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = (int32){v}; else bOk = false; }}"),
        Ty::U64 | Ty::Usize => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = (uint64){v}; else bOk = false; }}"),
        Ty::I64 => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = (int64){v}; else bOk = false; }}"),
        Ty::F64 => format!("{{ double {v}; if ({j}->TryGetNumber({v})) {target} = {v}; else bOk = false; }}"),
        Ty::Str => format!("{{ FString {v}; if ({j}->TryGetString({v})) {target} = MoveTemp({v}); else bOk = false; }}"),
        Ty::Bytes => format!("if (!MusubiJson::BytesFromJson({j}, {target})) bOk = false;"),
        Ty::Uuid => format!("{{ FString {v}; if ({j}->TryGetString({v}) && FMusubiUuid::Parse({v}, {target})) {{}} else bOk = false; }}"),
        Ty::Json => format!("{target} = FMusubiJson({j});"),
        Ty::Opt(i) => format!(
            "if ({j}->IsNull()) {target}.Reset(); else {{ {} {v}{{}}; {} {target} = MoveTemp({v}); }}",
            cpp_ty(model, i),
            from_json(model, i, &v, j, depth + 1)
        ),
        Ty::Vec(i) => format!(
            "{{ const TArray<TSharedPtr<FJsonValue>>* {v}A; if ({j}->TryGetArray({v}A)) {{ {target}.Reset({v}A->Num()); for (const TSharedPtr<FJsonValue>& {v}E : *{v}A) {{ {} {v}{{}}; {} {target}.Add(MoveTemp({v})); }} }} else bOk = false; }}",
            cpp_ty(model, i),
            from_json(model, i, &v, &format!("{v}E"), depth + 1)
        ),
        Ty::Map(val) => format!(
            "{{ const TSharedPtr<FJsonObject>* {v}O; if ({j}->TryGetObject({v}O)) {{ {target}.Reset(); for (const auto& {v}P : (*{v}O)->Values) {{ {} {v}{{}}; {} {target}.Add(FString(*{v}P.Key), MoveTemp({v})); }} }} else bOk = false; }}",
            cpp_ty(model, val),
            from_json(model, val, &v, &format!("{v}P.Value"), depth + 1)
        ),
        Ty::Named(n) => {
            if is_enum(model, n).map(|e| e.int_repr.is_some()).unwrap_or(false) {
                format!("{{ double {v}; if ({j}->TryGetNumber({v}) && FMusubi{n}FromInt((int32){v}, {target})) {{}} else bOk = false; }}")
            } else {
                format!("if (!FMusubi{n}::FromJson({j}, {target})) bOk = false;")
            }
        }
    }
}

fn pc_write(model: &Model, t: &Ty, expr: &str, pos_cm: bool, depth: usize) -> String {
    let v = format!("V{depth}");
    match t {
        Ty::Bool => format!("W.Bool({expr});"),
        Ty::U8 => format!("W.Byte({expr});"),
        Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => format!("W.Varint((uint64){expr});"),
        Ty::I32 => format!("W.Zigzag((int64){expr});"),
        Ty::I64 => format!("W.Zigzag({expr});"),
        Ty::F64 => {
            if pos_cm {
                format!("W.Zigzag((int64)MusubiPostcard::ToCm({expr}));")
            } else {
                format!("W.F64({expr});")
            }
        }
        Ty::Str => format!("W.Str({expr});"),
        Ty::Bytes => format!("W.Bytes({expr});"),
        Ty::Uuid => format!("W.Uuid({expr});"),
        Ty::Json => format!("W.Str({expr}.ToText());"),
        // `json_in_binary` serialises the whole Option as one JSON text: None is the text "null",
        // there is no postcard option marker.
        Ty::Opt(i) if matches!(**i, Ty::Json) => {
            format!("W.Str({expr}.IsSet() ? {expr}.GetValue().ToText() : FString(TEXT(\"null\")));")
        }
        Ty::Opt(i) => format!(
            "if ({expr}.IsSet()) {{ W.Byte(1); {} }} else W.Byte(0);",
            pc_write(model, i, &format!("{expr}.GetValue()"), pos_cm, depth + 1)
        ),
        Ty::Vec(i) => format!(
            "W.Varint((uint64){expr}.Num()); for (const {}& {v} : {expr}) {{ {} }}",
            cpp_ty(model, i),
            pc_write(model, i, &v, pos_cm, depth + 1)
        ),
        Ty::Map(val) => format!(
            "W.Varint((uint64){expr}.Num()); for (const auto& {v} : {expr}) {{ W.Str({v}.Key); {} }}",
            pc_write(model, val, &format!("{v}.Value"), pos_cm, depth + 1)
        ),
        Ty::Named(n) => {
            if is_enum(model, n).map(|e| e.int_repr.is_some()).unwrap_or(false) {
                format!("W.Zigzag((int64)(int32){expr});")
            } else {
                format!("FMusubi{n}::Write(W, {expr});")
            }
        }
    }
}

/// Upper bound on the elements a postcard reader pre-allocates from a length prefix; a hostile
/// or corrupt frame can claim 2^63 elements and must not turn into a multi-gigabyte `Reset`.
const PC_RESERVE_CAP: u64 = 1024;

fn pc_read(model: &Model, t: &Ty, target: &str, pos_cm: bool, depth: usize) -> String {
    let v = format!("V{depth}");
    match t {
        Ty::Bool => format!("{target} = R.Bool();"),
        Ty::U8 => format!("{target} = R.Byte();"),
        Ty::U16 => format!("{target} = (uint16)R.Varint();"),
        Ty::U32 => format!("{target} = (uint32)R.Varint();"),
        Ty::U64 | Ty::Usize => format!("{target} = R.Varint();"),
        Ty::I32 => format!("{target} = (int32)R.Zigzag();"),
        Ty::I64 => format!("{target} = R.Zigzag();"),
        Ty::F64 => {
            if pos_cm {
                format!("{target} = MusubiPostcard::FromCm((int32)R.Zigzag());")
            } else {
                format!("{target} = R.F64();")
            }
        }
        Ty::Str => format!("{target} = R.Str();"),
        Ty::Bytes => format!("R.Bytes({target});"),
        Ty::Uuid => format!("{target} = R.Uuid();"),
        Ty::Json => format!("{target} = FMusubiJson::FromText(R.Str());"),
        Ty::Opt(i) if matches!(**i, Ty::Json) => format!(
            "{{ FString {v} = R.Str(); if ({v} == TEXT(\"null\")) {target}.Reset(); else {target} = FMusubiJson::FromText({v}); }}"
        ),
        // postcard encodes Option as one byte 0/1; anything else is a corrupt frame, not None
        Ty::Opt(i) => format!(
            "{{ uint8 {v}T = R.Byte(); if ({v}T == 1) {{ {} {v}{{}}; {} {target} = MoveTemp({v}); }} else {{ if ({v}T != 0) R.Fail(); {target}.Reset(); }} }}",
            cpp_ty(model, i),
            pc_read(model, i, &v, pos_cm, depth + 1)
        ),
        // the length prefix is peer-controlled: reserve at most PC_RESERVE_CAP elements up front,
        // the loop still stops at the real end of the frame via R.Ok()
        Ty::Vec(i) => format!(
            "{{ uint64 {v}N = R.Len(); {target}.Reset((int32)({v}N < {PC_RESERVE_CAP} ? {v}N : {PC_RESERVE_CAP})); for (uint64 {v}I = 0; {v}I < {v}N && R.Ok(); ++{v}I) {{ {} {v}{{}}; {} {target}.Add(MoveTemp({v})); }} }}",
            cpp_ty(model, i),
            pc_read(model, i, &v, pos_cm, depth + 1)
        ),
        Ty::Map(val) => format!(
            "{{ uint64 {v}N = R.Len(); {target}.Reset(); for (uint64 {v}I = 0; {v}I < {v}N && R.Ok(); ++{v}I) {{ FString {v}K = R.Str(); {} {v}{{}}; {} {target}.Add(MoveTemp({v}K), MoveTemp({v})); }} }}",
            cpp_ty(model, val),
            pc_read(model, val, &v, pos_cm, depth + 1)
        ),
        Ty::Named(n) => {
            if is_enum(model, n).map(|e| e.int_repr.is_some()).unwrap_or(false) {
                format!("if (!FMusubi{n}FromInt((int32)R.Zigzag(), {target})) R.Fail();")
            } else {
                format!("FMusubi{n}::Read(R, {target});")
            }
        }
    }
}

fn field_decl(model: &Model, f: &Field) -> String {
    let ty = cpp_ty(model, &f.ty);
    let init = match (&f.ty, f.default_fn.as_deref()) {
        // `#[serde(default = "fn")]`: the C++ default must equal what the Rust fn returns.
        (Ty::F64, Some("one_f64")) => " = 1.0",
        (Ty::Bool, Some("yes")) => " = true",
        (Ty::U32, Some("one")) => " = 1",
        (Ty::U32, Some("default_party_size")) => " = 4",
        (Ty::U32, Some("default_party_list_limit")) => " = 50",
        (Ty::Str, Some("default_query")) => " = TEXT(\"*\")",
        (Ty::U64, Some("default_colocate_ttl")) => " = 300",
        (Ty::Vec(_), Some("Vec::new")) => "",
        (_, Some(other)) => panic!(
            "field {}: unknown serde default fn `{other}` — teach cpp.rs its value",
            f.name
        ),
        (Ty::Bool, None) => " = false",
        (Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize | Ty::I32 | Ty::I64, None) => " = 0",
        (Ty::F64, None) => " = 0.0",
        _ => "",
    };
    format!("\t{ty} {}{init};\n", f.name)
}

/// JSON object key for a field (`#[serde(rename)]` wins over the Rust name).
fn wire_key(f: &Field) -> &str {
    f.rename.as_deref().unwrap_or(&f.name)
}

fn emit_struct_decl(model: &Model, h: &mut String, name: &str, fields: &[Field]) {
    let _ = writeln!(h, "struct MUSUBI_API FMusubi{name}\n{{");
    for f in fields {
        h.push_str(&field_decl(model, f));
    }
    let _ = writeln!(
        h,
        "\n\tstatic TSharedPtr<FJsonValue> ToJson(const FMusubi{name}& V);\n\tstatic bool FromJson(const TSharedPtr<FJsonValue>& J, FMusubi{name}& Out);\n\tstatic void Write(FMusubiPostcardWriter& W, const FMusubi{name}& V);\n\tstatic void Read(FMusubiPostcardReader& R, FMusubi{name}& Out);\n}};\n"
    );
}

fn emit_struct_impl(c: &mut String, model: &Model, name: &str, fields: &[Field]) {
    let _ = writeln!(c, "TSharedPtr<FJsonValue> FMusubi{name}::ToJson(const FMusubi{name}& V)\n{{\n\tTSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();");
    for f in fields {
        let value = to_json(model, &f.ty, &format!("V.{}", f.name), f.pos_cm);
        let present = match (&f.ty, f.skip_if_default) {
            (_, false) => None,
            (Ty::Opt(_), true) => Some(format!("V.{}.IsSet()", f.name)),
            (Ty::Bool, true) => Some(format!("V.{}", f.name)),
            (
                Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize | Ty::I32 | Ty::I64 | Ty::F64,
                true,
            ) => Some(format!("V.{} != 0", f.name)),
            (Ty::Vec(_) | Ty::Map(_), true) => Some(format!("V.{}.Num() > 0", f.name)),
            (other, true) => panic!(
                "field {}: skip_serializing_if on {other:?} — teach cpp.rs the emptiness test",
                f.name
            ),
        };
        match present {
            Some(cond) => {
                let _ = writeln!(
                    c,
                    "\tif ({cond}) O->SetField(TEXT(\"{}\"), {value});",
                    wire_key(f)
                );
            }
            None => {
                let _ = writeln!(c, "\tO->SetField(TEXT(\"{}\"), {value});", wire_key(f));
            }
        }
    }
    let _ = writeln!(c, "\treturn MakeShared<FJsonValueObject>(O);\n}}\n");

    let _ = writeln!(c, "bool FMusubi{name}::FromJson(const TSharedPtr<FJsonValue>& J, FMusubi{name}& Out)\n{{\n\tconst TSharedPtr<FJsonObject>* OP;\n\tif (!J.IsValid() || !J->TryGetObject(OP)) return false;\n\tconst FJsonObject& O = **OP;\n\tbool bOk = true;");
    for f in fields {
        let read = from_json(model, &f.ty, &format!("Out.{}", f.name), "F", 0);
        if f.default || matches!(f.ty, Ty::Opt(_)) {
            let _ = writeln!(c, "\tif (TSharedPtr<FJsonValue> F = O.TryGetField(TEXT(\"{}\")); F.IsValid() && !F->IsNull()) {{ {read} }}", wire_key(f));
        } else {
            let _ = writeln!(c, "\tif (TSharedPtr<FJsonValue> F = O.TryGetField(TEXT(\"{}\")); F.IsValid()) {{ {read} }} else bOk = false;", wire_key(f));
        }
    }
    let _ = writeln!(c, "\treturn bOk;\n}}\n");

    let _ = writeln!(
        c,
        "void FMusubi{name}::Write(FMusubiPostcardWriter& W, const FMusubi{name}& V)\n{{"
    );
    for f in fields {
        let _ = writeln!(
            c,
            "\t{}",
            pc_write(model, &f.ty, &format!("V.{}", f.name), f.pos_cm, 0)
        );
    }
    let _ = writeln!(c, "}}\n");

    let _ = writeln!(
        c,
        "void FMusubi{name}::Read(FMusubiPostcardReader& R, FMusubi{name}& Out)\n{{"
    );
    for f in fields {
        let _ = writeln!(
            c,
            "\t{}",
            pc_read(model, &f.ty, &format!("Out.{}", f.name), f.pos_cm, 0)
        );
    }
    let _ = writeln!(c, "}}\n");
}

/// Externally tagged enum → one struct per data-carrying variant + an `E…Tag` enum + a tagged
/// union struct holding `TOptional<>` payloads (plain C++, no std::variant: Unreal builds
/// without the STL in some configs).
fn emit_enum(h: &mut String, c: &mut String, model: &Model, e: &Enum) {
    let n = &e.name;
    if let Some(vals) = &e.int_repr {
        let _ = writeln!(h, "enum class EMusubi{n} : int32\n{{");
        for (name, v) in vals {
            let _ = writeln!(h, "\t{name} = {v},");
        }
        let _ = writeln!(
            h,
            "}};\nMUSUBI_API bool FMusubi{n}FromInt(int32 V, EMusubi{n}& Out);\n"
        );
        let _ = writeln!(
            c,
            "bool FMusubi{n}FromInt(int32 V, EMusubi{n}& Out)\n{{\n\tswitch (V)\n\t{{"
        );
        for (name, v) in vals {
            let _ = writeln!(c, "\tcase {v}: Out = EMusubi{n}::{name}; return true;");
        }
        let _ = writeln!(c, "\tdefault: return false;\n\t}}\n}}\n");
        return;
    }

    let _ = writeln!(h, "enum class EMusubi{n}Tag : uint8\n{{");
    for (i, v) in e.variants.iter().enumerate() {
        let _ = writeln!(h, "\t{} = {i},", v.name);
    }
    let _ = writeln!(h, "}};\nMUSUBI_API const TCHAR* MusubiTagName(EMusubi{n}Tag Tag);\nMUSUBI_API bool MusubiTagFromName(const FString& Name, EMusubi{n}Tag& Out);\n");

    let _ = writeln!(
        c,
        "const TCHAR* MusubiTagName(EMusubi{n}Tag Tag)\n{{\n\tswitch (Tag)\n\t{{"
    );
    for v in &e.variants {
        let _ = writeln!(
            c,
            "\tcase EMusubi{n}Tag::{}: return TEXT(\"{}\");",
            v.name, v.tag
        );
    }
    let _ = writeln!(c, "\t}}\n\treturn TEXT(\"\");\n}}\n");
    let _ = writeln!(
        c,
        "bool MusubiTagFromName(const FString& Name, EMusubi{n}Tag& Out)\n{{"
    );
    for v in &e.variants {
        let _ = writeln!(
            c,
            "\tif (Name == TEXT(\"{}\")) {{ Out = EMusubi{n}Tag::{}; return true; }}",
            v.tag, v.name
        );
    }
    let _ = writeln!(c, "\treturn false;\n}}\n");

    for v in &e.variants {
        if let VariantKind::Struct(fields) = &v.kind {
            emit_struct_decl(model, h, &format!("{n}{}", v.name), fields);
            emit_struct_impl(c, model, &format!("{n}{}", v.name), fields);
        }
    }

    let is_unit_only = e
        .variants
        .iter()
        .all(|v| matches!(v.kind, VariantKind::Unit));
    let _ = writeln!(
        h,
        "struct MUSUBI_API FMusubi{n}\n{{\n\tEMusubi{n}Tag Tag = EMusubi{n}Tag::{};",
        e.variants[0].name
    );
    if !is_unit_only {
        for v in &e.variants {
            match &v.kind {
                VariantKind::Unit => {}
                VariantKind::Newtype(t) => {
                    let _ = writeln!(h, "\tTOptional<{}> {};", cpp_ty(model, t), v.name);
                }
                VariantKind::Struct(_) => {
                    let _ = writeln!(h, "\tTOptional<FMusubi{n}{}> {};", v.name, v.name);
                }
            }
        }
    }
    let _ = writeln!(h, "\n\tstatic TSharedPtr<FJsonValue> ToJson(const FMusubi{n}& V);\n\tstatic bool FromJson(const TSharedPtr<FJsonValue>& J, FMusubi{n}& Out);\n\tstatic void Write(FMusubiPostcardWriter& W, const FMusubi{n}& V);\n\tstatic void Read(FMusubiPostcardReader& R, FMusubi{n}& Out);\n}};\n");

    if let Some(tag_key) = &e.internal_tag {
        emit_internal_tag_impl(c, model, e, tag_key);
        return;
    }
    // ToJson: unit → "tag"; data → {"tag": payload}
    let _ = writeln!(c, "TSharedPtr<FJsonValue> FMusubi{n}::ToJson(const FMusubi{n}& V)\n{{\n\tswitch (V.Tag)\n\t{{");
    for v in &e.variants {
        let vn = &v.name;
        match &v.kind {
            VariantKind::Unit => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: return MakeShared<FJsonValueString>(TEXT(\"{}\"));", v.tag);
            }
            VariantKind::Newtype(t) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: return MusubiJson::Tagged(TEXT(\"{}\"), V.{vn}.IsSet() ? TSharedPtr<FJsonValue>({}) : MusubiJson::Null());", v.tag, to_json(model, t, &format!("V.{vn}.GetValue()"), false));
            }
            VariantKind::Struct(_) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: return MusubiJson::Tagged(TEXT(\"{}\"), V.{vn}.IsSet() ? FMusubi{n}{vn}::ToJson(V.{vn}.GetValue()) : MusubiJson::Null());", v.tag);
            }
        }
    }
    let _ = writeln!(c, "\t}}\n\treturn MusubiJson::Null();\n}}\n");

    let _ = writeln!(c, "bool FMusubi{n}::FromJson(const TSharedPtr<FJsonValue>& J, FMusubi{n}& Out)\n{{\n\tFString Tag;\n\tTSharedPtr<FJsonValue> P;\n\tif (!MusubiJson::Untag(J, Tag, P)) return false;\n\tif (!MusubiTagFromName(Tag, Out.Tag)) return false;\n\tbool bOk = true;\n\tswitch (Out.Tag)\n\t{{");
    for v in &e.variants {
        let vn = &v.name;
        match &v.kind {
            VariantKind::Unit => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: break;");
            }
            VariantKind::Newtype(t) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: {{ if (!P.IsValid()) return false; {} V0{{}}; {} Out.{vn} = MoveTemp(V0); break; }}", cpp_ty(model, t), from_json(model, t, "V0", "P", 1));
            }
            VariantKind::Struct(_) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: {{ FMusubi{n}{vn} V0{{}}; if (!FMusubi{n}{vn}::FromJson(P, V0)) bOk = false; Out.{vn} = MoveTemp(V0); break; }}");
            }
        }
    }
    let _ = writeln!(c, "\t}}\n\treturn bOk;\n}}\n");

    // postcard: varint variant index, then payload fields in order
    let _ = writeln!(c, "void FMusubi{n}::Write(FMusubiPostcardWriter& W, const FMusubi{n}& V)\n{{\n\tW.Varint((uint64)V.Tag);\n\tswitch (V.Tag)\n\t{{");
    for v in &e.variants {
        let vn = &v.name;
        match &v.kind {
            VariantKind::Unit => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: break;");
            }
            VariantKind::Newtype(t) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: if (V.{vn}.IsSet()) {{ {} }} else W.Fail(); break;", pc_write(model, t, &format!("V.{vn}.GetValue()"), false, 0));
            }
            VariantKind::Struct(_) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: if (V.{vn}.IsSet()) FMusubi{n}{vn}::Write(W, V.{vn}.GetValue()); else W.Fail(); break;");
            }
        }
    }
    let _ = writeln!(c, "\t}}\n}}\n");

    let _ = writeln!(c, "void FMusubi{n}::Read(FMusubiPostcardReader& R, FMusubi{n}& Out)\n{{\n\tuint64 Idx = R.Varint();\n\tif (Idx >= {}) {{ R.Fail(); return; }}\n\tOut.Tag = (EMusubi{n}Tag)Idx;\n\tswitch (Out.Tag)\n\t{{", e.variants.len());
    for v in &e.variants {
        let vn = &v.name;
        match &v.kind {
            VariantKind::Unit => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: break;");
            }
            VariantKind::Newtype(t) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: {{ {} V0{{}}; {} Out.{vn} = MoveTemp(V0); break; }}", cpp_ty(model, t), pc_read(model, t, "V0", false, 1));
            }
            VariantKind::Struct(_) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: {{ FMusubi{n}{vn} V0{{}}; FMusubi{n}{vn}::Read(R, V0); Out.{vn} = MoveTemp(V0); break; }}");
            }
        }
    }
    let _ = writeln!(c, "\t}}\n}}\n");
}

/// Internally tagged enum (`#[serde(tag = "type")]`): every variant is one JSON object with the
/// tag as a key next to its fields. JSON only — postcard `Write`/`Read` fail (the simulator
/// stream is JSON text).
fn emit_internal_tag_impl(c: &mut String, model: &Model, e: &Enum, tag_key: &str) {
    let _ = model;
    let n = &e.name;
    let _ = writeln!(c, "TSharedPtr<FJsonValue> FMusubi{n}::ToJson(const FMusubi{n}& V)\n{{\n\tTSharedPtr<FJsonValue> P;\n\tswitch (V.Tag)\n\t{{");
    for v in &e.variants {
        let vn = &v.name;
        match &v.kind {
            VariantKind::Unit => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: P = MakeShared<FJsonValueObject>(MakeShared<FJsonObject>()); break;");
            }
            VariantKind::Struct(_) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: P = V.{vn}.IsSet() ? FMusubi{n}{vn}::ToJson(V.{vn}.GetValue()) : TSharedPtr<FJsonValue>(MakeShared<FJsonValueObject>(MakeShared<FJsonObject>())); break;");
            }
            VariantKind::Newtype(_) => unreachable!("rejected in model::lift"),
        }
    }
    let _ = writeln!(c, "\t}}\n\tif (P.IsValid() && P->Type == EJson::Object) P->AsObject()->SetStringField(TEXT(\"{tag_key}\"), MusubiTagName(V.Tag));\n\treturn P;\n}}\n");
    let other = e.variants.iter().find(|v| v.other);
    let _ = writeln!(c, "bool FMusubi{n}::FromJson(const TSharedPtr<FJsonValue>& J, FMusubi{n}& Out)\n{{\n\tconst TSharedPtr<FJsonObject>* OP;\n\tif (!J.IsValid() || !J->TryGetObject(OP)) return false;\n\tFString Tag;\n\tif (!(*OP)->TryGetStringField(TEXT(\"{tag_key}\"), Tag)) return false;\n\tOut = FMusubi{n}();\n\tif (!MusubiTagFromName(Tag, Out.Tag))\n\t{{");
    match other {
        Some(o) => {
            let _ = writeln!(
                c,
                "\t\tOut.Tag = EMusubi{n}Tag::{};\n\t\treturn true;",
                o.name
            );
        }
        None => {
            let _ = writeln!(c, "\t\treturn false;");
        }
    }
    let _ = writeln!(c, "\t}}\n\tbool bOk = true;\n\tswitch (Out.Tag)\n\t{{");
    for v in &e.variants {
        let vn = &v.name;
        match &v.kind {
            VariantKind::Unit => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: break;");
            }
            VariantKind::Struct(_) => {
                let _ = writeln!(c, "\tcase EMusubi{n}Tag::{vn}: {{ FMusubi{n}{vn} V0{{}}; if (!FMusubi{n}{vn}::FromJson(J, V0)) bOk = false; Out.{vn} = MoveTemp(V0); break; }}");
            }
            VariantKind::Newtype(_) => unreachable!(),
        }
    }
    let _ = writeln!(c, "\t}}\n\treturn bOk;\n}}\n");
    let _ = writeln!(c, "void FMusubi{n}::Write(FMusubiPostcardWriter& W, const FMusubi{n}&)\n{{\n\tW.Fail();\n}}\n");
    let _ = writeln!(
        c,
        "void FMusubi{n}::Read(FMusubiPostcardReader& R, FMusubi{n}&)\n{{\n\tR.Fail();\n}}\n"
    );
}

/// `api`: export macro of the owning module; `header`: file name of the generated header (the
/// .cpp includes it); `extra_includes`: headers the types depend on (e.g. the core wire file).
pub fn emit(
    model: &Model,
    source_note: &str,
    api: &str,
    header: &str,
    extra_includes: &[&str],
) -> Out {
    let mut h = String::new();
    let mut c = String::new();
    let banner = format!(
        "// GENERATED by `cargo run -p musubi-unreal-gen` from {source_note}. Do not edit.\n"
    );
    h.push_str(&banner);
    h.push_str("#pragma once\n\n#include \"CoreMinimal.h\"\n#include \"Dom/JsonValue.h\"\n#include \"Dom/JsonObject.h\"\n#include \"MusubiJson.h\"\n#include \"MusubiPostcard.h\"\n#include \"MusubiWireTypes.h\"\n\n");
    c.push_str(&banner);
    if !extra_includes.is_empty() {
        for inc in extra_includes {
            let _ = writeln!(h, "#include \"{inc}\"");
        }
        h.push('\n');
    }
    let _ = writeln!(c, "#include \"{header}\"\n");

    // forward declarations so structs can reference each other in any order
    for s in &model.structs {
        let _ = writeln!(h, "struct FMusubi{};", s.name);
    }
    for e in &model.enums {
        if e.int_repr.is_none() {
            let _ = writeln!(h, "struct FMusubi{};", e.name);
        }
    }
    h.push('\n');

    // int-repr enums first (plain values), then plain structs, then tagged enums (which embed structs)
    for e in model.enums.iter().filter(|e| e.int_repr.is_some()) {
        emit_enum(&mut h, &mut c, model, e);
    }
    for e in model.enums.iter().filter(|e| {
        e.int_repr.is_none()
            && e.variants
                .iter()
                .all(|v| matches!(v.kind, VariantKind::Unit))
    }) {
        emit_enum(&mut h, &mut c, model, e);
    }
    // structs and data-carrying enums in dependency order: a by-value field (TOptional/TArray of a
    // generated type) needs the complete type, so whatever a type names comes out before it
    enum Item<'a> {
        S(&'a Struct),
        E(&'a Enum),
    }
    let mut pending: Vec<Item> = model
        .structs
        .iter()
        .map(Item::S)
        .chain(
            model
                .enums
                .iter()
                .filter(|e| {
                    e.int_repr.is_none()
                        && !e
                            .variants
                            .iter()
                            .all(|v| matches!(v.kind, VariantKind::Unit))
                })
                .map(Item::E),
        )
        .collect();
    let mut done: Vec<String> = model
        .enums
        .iter()
        .filter(|e| {
            e.int_repr.is_some()
                || e.variants
                    .iter()
                    .all(|v| matches!(v.kind, VariantKind::Unit))
        })
        .map(|e| e.name.clone())
        .collect();
    let in_model = |n: &str| {
        model.structs.iter().any(|s| s.name == n) || model.enums.iter().any(|e| e.name == n)
    };
    while !pending.is_empty() {
        let ready = pending.iter().position(|it| {
            let deps: Vec<String> = match it {
                Item::S(s) => s.fields.iter().flat_map(|f| named_deps(&f.ty)).collect(),
                Item::E(e) => e
                    .variants
                    .iter()
                    .flat_map(|v| match &v.kind {
                        VariantKind::Unit => vec![],
                        VariantKind::Newtype(t) => named_deps(t),
                        VariantKind::Struct(fs) => {
                            fs.iter().flat_map(|f| named_deps(&f.ty)).collect()
                        }
                    })
                    .collect(),
            };
            deps.iter().all(|d| !in_model(d) || done.contains(d))
        });
        // a cycle (none today) falls back to declaration order; the forward declarations cover it
        let idx = ready.unwrap_or(0);
        match pending.remove(idx) {
            Item::S(s) => {
                emit_struct_decl(model, &mut h, &s.name, &s.fields);
                emit_struct_impl(&mut c, model, &s.name, &s.fields);
                done.push(s.name.clone());
            }
            Item::E(e) => {
                emit_enum(&mut h, &mut c, model, e);
                done.push(e.name.clone());
            }
        }
    }
    Out {
        header: h.replace("MUSUBI_API", api),
        cpp: c,
    }
}

/// Generated type names a field type refers to (through Option/Vec/Map).
fn named_deps(t: &Ty) -> Vec<String> {
    match t {
        Ty::Named(n) => vec![n.clone()],
        Ty::Opt(i) | Ty::Vec(i) | Ty::Map(i) => named_deps(i),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model {
            structs: vec![Struct {
                name: "Pkt".into(),
                fields: vec![
                    Field {
                        name: "ids".into(),
                        ty: Ty::Vec(Box::new(Ty::U32)),
                        rename: None,
                        default: false,
                        default_fn: None,
                        skip_if_default: false,
                        pos_cm: false,
                    },
                    Field {
                        name: "note".into(),
                        ty: Ty::Opt(Box::new(Ty::Str)),
                        rename: None,
                        default: false,
                        default_fn: None,
                        skip_if_default: false,
                        pos_cm: false,
                    },
                ],
            }],
            enums: vec![],
        }
    }

    #[test]
    fn postcard_reader_bounds_preallocation_and_rejects_bad_option_tags() {
        let out = emit(&model(), "test", "TEST_API", "Test.h", &[]);
        let read = out
            .cpp
            .split("void FMusubiPkt::Read(")
            .nth(1)
            .expect("Read impl");
        assert!(
            read.contains(&format!(
                "(V0N < {PC_RESERVE_CAP} ? V0N : {PC_RESERVE_CAP})"
            )),
            "{read}"
        );
        assert!(!read.contains("Reset((int32)V0N)"), "{read}");
        assert!(read.contains("if (V0T != 0) R.Fail();"), "{read}");
    }
}
