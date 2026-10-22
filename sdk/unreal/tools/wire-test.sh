#!/usr/bin/env bash
# Builds and runs the golden-vector test against the UE shim (no Unreal toolchain needed).
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=Musubi/Source/Musubi
CXX=${CXX:-g++}
OUT=$(mktemp -d)
"$CXX" -std=c++17 -O1 -Wall -Wno-unused-parameter -I tools/ue-shim -I "$SRC/Public" \
  tools/wire-test.cpp "$SRC/Private/MusubiWireTypes.cpp" "$SRC/Private/MusubiJson.cpp" \
  "$SRC/Private/MusubiPostcard.cpp" "$SRC/Private/MusubiWire.generated.cpp" "$SRC/Private/MusubiEnvelope.cpp" -o "$OUT/wire-test"
"$OUT/wire-test" tools/wire-vectors.json
