#!/usr/bin/env bash
# Real UBT/UAT BuildPlugin of sdk/unreal/Musubi inside an Epic dev-slim image (requires `docker login ghcr.io`
# with a GitHub account linked to Epic Games). Output + UBT logs land in target/ue/<ver>/.
#   tools/ue-build.sh 5.3.2            # BuildPlugin (Editor + Game, Linux) for one engine version
#   UE_TARGETS=Linux UE_EXTRA="-StrictIncludes" tools/ue-build.sh 5.7.4
set -euo pipefail
VER="${1:?engine version, e.g. 5.3.2}"
IMG="${UE_IMAGE:-ghcr.io/epicgames/unreal-engine:dev-slim-$VER}"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="${UE_OUT:-$ROOT/target/ue/$VER}"
UE_ROOT="${UE_ROOT:-/home/ue4/UnrealEngine}"
UE_MINOR="$(echo "$VER" | cut -d. -f2)"
mkdir -p "$OUT/home"

# 5.4+ UBT runs compiles through Unreal Build Accelerator; its detours (libUbaDetours.so) crash clang 20 in opendir on
# every Game-target action in the 5.8.4 image ("[CHAINED-SIGNAL 11]" spam, 200 MB log, hours instead of minutes).
# With bAllowUBAExecutor=false UBT still reports a "local UBA executor" but the crashes stop and the Game target takes
# ~40 s (verified both ways on 5.8.4). bAllowUBALocalExecutor is deprecated in 5.8 (warning only). Neither field exists
# on 5.3, so only write the XML where UBA exists.
if [ "$UE_MINOR" -ge 4 ]; then
  mkdir -p "$OUT/home/.config/Unreal Engine/UnrealBuildTool"
  cat > "$OUT/home/.config/Unreal Engine/UnrealBuildTool/BuildConfiguration.xml" <<'XML'
<?xml version="1.0" encoding="utf-8" ?>
<Configuration xmlns="https://www.unrealengine.com/BuildConfiguration">
  <BuildConfiguration>
    <bAllowUBAExecutor>false</bAllowUBAExecutor>
    <bAllowUBALocalExecutor>false</bAllowUBALocalExecutor>
  </BuildConfiguration>
</Configuration>
XML
fi

docker run --rm -u "$(id -u):$(id -g)" -e HOME=/out/home \
  -v "$ROOT/sdk/unreal/Musubi:/src/Musubi:ro" -v "$OUT:/out" "$IMG" bash -c "
    set -e; rm -rf /out/Plugin /out/Package/HostProject/Plugins; mkdir -p /out/Plugin && cp -r /src/Musubi/. /out/Plugin/
    # UBT resolves its per-user config from the passwd home (/home/ue4), not \$HOME, and also reads the host project's Saved/.
    X='/out/home/.config/Unreal Engine/UnrealBuildTool/BuildConfiguration.xml'
    if [ -f \"\$X\" ]; then for D in '/home/ue4/.config/Unreal Engine/UnrealBuildTool' /out/Package/HostProject/Saved/UnrealBuildTool; do
      mkdir -p \"\$D\" 2>/dev/null && cp \"\$X\" \"\$D/\" || echo \"ue-build: cannot write \$D\"; done; fi
    set -o pipefail
    $UE_ROOT/Engine/Build/BatchFiles/RunUAT.sh BuildPlugin \
      -Plugin=/out/Plugin/Musubi.uplugin -Package=/out/Package \
      -TargetPlatforms=${UE_TARGETS:-Linux} -Rocket ${UE_EXTRA:-} 2>&1 | tee /out/build.log | grep -E 'error|warning: |BUILD|Took|Result' || rc=\${PIPESTATUS[0]}
    tail -3 /out/build.log; exit \${rc:-0}"
