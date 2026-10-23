#!/usr/bin/env bash
# Build the demo project (Editor target, used headless as server and client like tools/nitl.sh) inside an Epic dev-slim image.
#   tools/demo-build.sh 5.8.4 [UBT target, default MusubiThirdPersonEditor]
set -euo pipefail
VER="${1:?engine version}"; TGT="${2:-MusubiThirdPersonEditor}"
IMG="${UE_IMAGE:-ghcr.io/epicgames/unreal-engine:dev-slim-$VER}"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"; OUT="$ROOT/target/ue/demo-$VER"; PROJ="$OUT/MusubiThirdPerson"
mkdir -p "$OUT/home/.config/Unreal Engine/UnrealBuildTool"
SRC="$ROOT/sdk/unreal/Demo/MusubiThirdPerson"; mkdir -p "$PROJ"
for D in Config Source Content; do rm -rf "$PROJ/$D"; cp -r "$SRC/$D" "$PROJ/$D"; done
cp "$SRC/MusubiThirdPerson.uproject" "$PROJ/"
rm -rf "$PROJ/Plugins/Musubi"; mkdir -p "$PROJ/Plugins"; cp -r "$ROOT/sdk/unreal/Musubi" "$PROJ/Plugins/Musubi"
# stamp the plugin with the engine it is built for, as UAT BuildPlugin does: a mismatching EngineVersion makes the editor ask
# "'Musubi' is Incompatible ... load anyway?" and -unattended answers No, so the plugin's modules are silently skipped
sed -i "s/\"EngineVersion\": \"[0-9.]*\"/\"EngineVersion\": \"${VER%.*}.0\"/" "$PROJ/Plugins/Musubi/Musubi.uplugin"
sed -i "s/\"EngineAssociation\": \"[0-9.]*\"/\"EngineAssociation\": \"${VER%.*}\"/" "$PROJ/MusubiThirdPerson.uproject"
printf '<?xml version="1.0" encoding="utf-8" ?>\n<Configuration xmlns="https://www.unrealengine.com/BuildConfiguration">\n  <BuildConfiguration>\n    <bAllowUBAExecutor>false</bAllowUBAExecutor>\n    <bAllowUBALocalExecutor>false</bAllowUBALocalExecutor>\n  </BuildConfiguration>\n</Configuration>\n' > "$OUT/home/.config/Unreal Engine/UnrealBuildTool/BuildConfiguration.xml"
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/out/home -v "$OUT:/out" "$IMG" bash -c "
  set -o pipefail; mkdir -p '/home/ue4/.config/Unreal Engine/UnrealBuildTool' 2>/dev/null && cp '/out/home/.config/Unreal Engine/UnrealBuildTool/BuildConfiguration.xml' '/home/ue4/.config/Unreal Engine/UnrealBuildTool/' || true
  /home/ue4/UnrealEngine/Engine/Build/BatchFiles/Linux/Build.sh $TGT Linux Development -Project=/out/MusubiThirdPerson/MusubiThirdPerson.uproject -NoUBA 2>&1 | tee /out/build.log | grep -E 'error|warning: |Total|Result|succeeded|failed' || rc=\${PIPESTATUS[0]}
  tail -3 /out/build.log; exit \${rc:-0}"
