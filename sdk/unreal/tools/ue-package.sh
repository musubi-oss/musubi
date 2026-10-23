#!/usr/bin/env bash
# Release zip of the prebuilt plugin from a tools/ue-build.sh run (Epic "Rocket" layout: Binaries + Intermediate/Build/*/Inc
# + Source + .uplugin — what a project drops into Plugins/ without recompiling). Objects, .debug files and the host project
# are left out; .uplugin EngineVersion is pinned to the engine it was built with.
#   tools/ue-package.sh 5.3.2 5.7.4 5.8.4           # needs target/ue/<ver>/Package from ue-build.sh
#   UE_BUILD=1 tools/ue-package.sh 5.8.4           # build first
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
[ $# -gt 0 ] || { echo "usage: $0 <engine version>..." >&2; exit 2; }
for VER in "$@"; do
  PKG="$ROOT/target/ue/$VER/Package"
  if [ ! -f "$PKG/Musubi.uplugin" ] || [ "${UE_BUILD:-0}" = 1 ]; then "$ROOT/sdk/unreal/tools/ue-build.sh" "$VER"; fi
  grep -q "AutomationTool exiting with ExitCode=0" "$ROOT/target/ue/$VER/build.log" || { echo "$VER: last build was not green" >&2; exit 1; }
  MM="${VER%.*}"
  PV="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["VersionName"])' "$PKG/Musubi.uplugin")"
  PLATS="$(ls "$PKG/Binaries" | paste -sd- -)"
  STAGE="$ROOT/target/ue/$VER/Stage/Musubi"; rm -rf "$STAGE"; mkdir -p "$STAGE"
  cp -r "$PKG/Source" "$STAGE/"
  cp -r "$PKG/Binaries" "$STAGE/"; find "$STAGE/Binaries" -name '*.debug' -delete
  find "$STAGE/Source" -type f \( -name '*.o' -o -name '*.pdb' \) -delete
  # generated headers (Inc/) are part of the prebuilt layout; object files are not
  for INC in "$PKG"/Intermediate/Build/*/*/Inc; do
    [ -d "$INC" ] || continue; D="$STAGE/${INC#$PKG/}"; mkdir -p "$D"; cp -r "$INC/." "$D/"
  done
  python3 - "$PKG/Musubi.uplugin" "$STAGE/Musubi.uplugin" "$MM" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); d["EngineVersion"]=f"{sys.argv[3]}.0"
json.dump(d,open(sys.argv[2],"w"),indent="\t",ensure_ascii=False); open(sys.argv[2],"a").write("\n")
PY
  ZIP="$ROOT/target/ue/Musubi-$PV-ue$MM-$PLATS.zip"; rm -f "$ZIP"
  (cd "$STAGE/.." && zip -qr "$ZIP" Musubi)
  rm -rf "$STAGE"
  printf '%s  %s  %s\n' "$(sha256sum "$ZIP" | cut -c1-16)" "$(du -h "$ZIP" | cut -f1)" "$ZIP"
done
