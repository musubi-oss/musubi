#!/usr/bin/env bash
# Build crates/musubi-quic-ffi (static lib) and drop it where MusubiQuic.Build.cs looks for it.
# Usage: tools/build-quic-ffi.sh [--target <rust-triple>]   (default: host)
set -euo pipefail
cd "$(dirname "$0")/../../.."
TARGET=""
if [[ "${1:-}" == "--target" ]]; then TARGET="$2"; fi
ARGS=(build -p musubi-quic-ffi --release)
[[ -n "$TARGET" ]] && ARGS+=(--target "$TARGET")
# No LTO / embedded bitcode (the archive is consumed by UE's own clang, not by rustc), and the debug info is
# stripped: Rust std ships DWARF 5 and UE 5.3's Breakpad `dump_syms`, which UBT runs on every linked .so on
# Linux, aborts on it (seen as "Aborted (core dumped)" right after "Link (lld) libUnrealEditor-MusubiQuic.so").
CARGO_PROFILE_RELEASE_LTO=false RUSTFLAGS="${RUSTFLAGS:-} -C embed-bitcode=no" cargo "${ARGS[@]}"
OUT="target/${TARGET:+$TARGET/}release"
case "${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}" in
  *windows*) PLAT=Win64; SRC="$OUT/musubi_quic_ffi.lib"; DST=musubi_quic_ffi.lib ;;
  aarch64*linux*) PLAT=LinuxArm64; SRC="$OUT/libmusubi_quic_ffi.a"; DST=libmusubi_quic_ffi.a ;;
  *linux*) PLAT=Linux; SRC="$OUT/libmusubi_quic_ffi.a"; DST=libmusubi_quic_ffi.a ;;
  *apple-darwin*) PLAT=Mac; SRC="$OUT/libmusubi_quic_ffi.a"; DST=libmusubi_quic_ffi.a ;;
  *) echo "unsupported target ${TARGET}" >&2; exit 1 ;;
esac
LIBDIR="sdk/unreal/Musubi/Source/ThirdParty/MusubiQuicFfi/lib/$PLAT"
mkdir -p "$LIBDIR"
cp "$SRC" "$LIBDIR/$DST"
case "$PLAT" in
  Linux|LinuxArm64) strip --strip-debug "$LIBDIR/$DST" ;;
  Mac) strip -S "$LIBDIR/$DST" ;;
esac
echo "-> $LIBDIR/$DST ($(du -h "$LIBDIR/$DST" | cut -f1))"
