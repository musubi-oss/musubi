#!/usr/bin/env bash
# Build the Musubi plugin against an Epic dev-slim image (ghcr.io/epicgames/unreal-engine).
# Needs: docker login ghcr.io with a GitHub account linked to Epic Games.
#   tools/ue-build.sh 5.3.2            # BuildPlugin (Editor + Game, Linux) for one engine version
#   UE_TARGETS=Linux UE_EXTRA="-StrictIncludes" tools/ue-build.sh 5.7.4
set -euo pipefail
VER="${1:?engine version, e.g. 5.3.2}"
IMG="${UE_IMAGE:-ghcr.io/epicgames/unreal-engine:dev-slim-$VER}"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="${UE_OUT:-$ROOT/target/ue/$VER}"
UE_ROOT="${UE_ROOT:-/home/ue4/UnrealEngine}"
mkdir -p "$OUT"
# the image runs as ue4 (uid 1000); the plugin dir is copied so UBT's Intermediate/ lands in $OUT
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/out/home \
  -v "$ROOT/sdk/unreal/Musubi:/src/Musubi:ro" -v "$OUT:/out" "$IMG" bash -c "
    set -e; mkdir -p /out/home; rm -rf /out/Plugin /out/Package/HostProject/Plugins; mkdir -p /out/Plugin && cp -r /src/Musubi/. /out/Plugin/
    set -o pipefail
    $UE_ROOT/Engine/Build/BatchFiles/RunUAT.sh BuildPlugin \
      -Plugin=/out/Plugin/Musubi.uplugin -Package=/out/Package \
      -TargetPlatforms=${UE_TARGETS:-Linux} -Rocket ${UE_EXTRA:-} 2>&1 | tee /out/build.log | grep -E 'error|warning: |BUILD|Took|Result' || rc=\${PIPESTATUS[0]}
    tail -3 /out/build.log; exit \${rc:-0}"
