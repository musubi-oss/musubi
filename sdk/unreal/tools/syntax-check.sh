#!/usr/bin/env bash
# Compiles the Unreal GDK wire layer against the UE shim with a plain C++17 compiler.
# Catches typos in the generated code without an Unreal toolchain; UBT builds for
# 5.3/5.7/5.8 remain the authoritative check (docs/gdk-unreal.md §10).
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=Musubi/Source/Musubi
CXX=${CXX:-g++}
for f in MusubiWireTypes MusubiJson MusubiPostcard MusubiWire.generated MusubiEnvelope; do
  "$CXX" -std=c++17 -fsyntax-only -Wall -Wextra -Wno-unused-parameter -Wno-unused-variable \
    -I tools/ue-shim -I "$SRC/Public" "$SRC/Private/$f.cpp"
  echo "ok $f"
done
SRV=Musubi/Source/MusubiServer
"$CXX" -std=c++17 -fsyntax-only -Wall -Wextra -Wno-unused-parameter -Wno-unused-variable \
  -I tools/ue-shim -I "$SRC/Public" -I "$SRV/Public" "$SRV/Private/MusubiSimWire.generated.cpp"
echo "ok MusubiSimWire.generated"
