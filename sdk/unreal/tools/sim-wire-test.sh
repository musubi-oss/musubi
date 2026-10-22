#!/usr/bin/env bash
# Builds and runs the simulator-stream golden-vector test against the UE shim.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=Musubi/Source/Musubi
SRV=Musubi/Source/MusubiServer
CXX=${CXX:-g++}
OUT=$(mktemp -d)
"$CXX" -std=c++17 -O1 -Wall -Wno-unused-parameter -I tools/ue-shim -I "$SRC/Public" -I "$SRV/Public" \
  tools/sim-wire-test.cpp "$SRC/Private/MusubiWireTypes.cpp" "$SRC/Private/MusubiJson.cpp" \
  "$SRC/Private/MusubiPostcard.cpp" "$SRC/Private/MusubiWire.generated.cpp" \
  "$SRV/Private/MusubiSimWire.generated.cpp" "$SRV/Private/MusubiSimCommandFrame.cpp" -o "$OUT/sim-wire-test"
"$OUT/sim-wire-test" tools/sim-vectors.json
