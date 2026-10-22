#!/usr/bin/env bash
# Build crates/musubi-quic-ffi (static lib) and drop it where MusubiQuic.Build.cs looks for it.
# Usage: tools/build-quic-ffi.sh [--target <rust-triple>]   (default: host)
set -euo pipefail
cd "$(dirname "$0")/../../.."
TARGET=""
if [[ "${1:-}" == "--target" ]]; then TARGET="$2"; fi
ARGS=(build -p musubi-quic-ffi --release)
[[ -n "$TARGET" ]] && ARGS+=(--target "$TARGET")
cargo "${ARGS[@]}"
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
echo "-> $LIBDIR/$DST ($(du -h "$LIBDIR/$DST" | cut -f1))"
