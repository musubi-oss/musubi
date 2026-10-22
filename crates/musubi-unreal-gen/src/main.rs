//! `cargo run -p musubi-unreal-gen [-- --check]`: regenerate the Unreal GDK wire layer from
//! `musubi-proto`. `--check` exits 1 when the committed files are stale (CI).

mod cpp;
mod model;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

const PROTO_TYPES: &[&str] = &["UserPresence"];
const REALTIME_TYPES: &[&str] = &[
    "Channel",
    "ChannelKind",
    "WorldEntity",
    "WorldComponent",
    "MatchmakerServer",
    "MatchmakerUser",
    "ChannelMessage",
    "Reaction",
    "PartyListing",
    "ClientMessage",
    "ServerMessage",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn main() -> Result<()> {
    let check = std::env::args().any(|a| a == "--check");
    let root = root();
    let proto = root.join("crates/musubi-proto/src");
    let mut model = model::Model::default();
    model::lift(
        &std::fs::read_to_string(proto.join("../../musubi-core/src/error.rs"))?,
        &["Code"],
        &mut model,
    )
    .context("musubi-core/error.rs")?;
    model::lift(
        &std::fs::read_to_string(proto.join("lib.rs"))?,
        PROTO_TYPES,
        &mut model,
    )
    .context("lib.rs")?;
    model::lift(
        &std::fs::read_to_string(proto.join("realtime.rs"))?,
        REALTIME_TYPES,
        &mut model,
    )
    .context("realtime.rs")?;

    let found: Vec<&str> = model
        .structs
        .iter()
        .map(|s| s.name.as_str())
        .chain(model.enums.iter().map(|e| e.name.as_str()))
        .collect();
    for w in PROTO_TYPES
        .iter()
        .chain(REALTIME_TYPES)
        .chain(["Code"].iter())
    {
        if !found.contains(w) {
            bail!("type {w} not found in musubi-proto sources");
        }
    }

    let out = cpp::emit(&model, "crates/musubi-proto/src/{lib,realtime}.rs");
    let src = root.join("sdk/unreal/Musubi/Source/Musubi");
    let targets = [
        (src.join("Public/MusubiWire.generated.h"), out.header),
        (src.join("Private/MusubiWire.generated.cpp"), out.cpp),
    ];
    let mut stale = false;
    for (path, content) in targets {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current != content {
            if check {
                eprintln!("stale: {}", path.display());
                stale = true;
            } else {
                std::fs::write(&path, content).with_context(|| path.display().to_string())?;
                println!("wrote {}", path.display());
            }
        }
    }
    if stale {
        bail!("generated Unreal wire files are out of date; run `cargo run -p musubi-unreal-gen`");
    }
    println!(
        "{} structs, {} enums ({} client / {} server messages)",
        model.structs.len(),
        model.enums.len(),
        model
            .enums
            .iter()
            .find(|e| e.name == "ClientMessage")
            .map(|e| e.variants.len())
            .unwrap_or(0),
        model
            .enums
            .iter()
            .find(|e| e.name == "ServerMessage")
            .map(|e| e.variants.len())
            .unwrap_or(0),
    );
    Ok(())
}
