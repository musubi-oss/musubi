#!/usr/bin/env bash
# Populate Demo/MusubiThirdPerson/Content with Epic's Third Person template content (not redistributable, so not in git).
#   tools/demo-content.sh /path/to/UnrealEngine            # an installed engine or a source checkout with Templates/ populated
#   tools/demo-content.sh --gitdeps /path/to/Commit.gitdeps.xml   # fetch straight from Epic's CDN (source checkout without Setup.sh)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"; DST="$ROOT/Demo/MusubiThirdPerson/Content"
mkdir -p "$DST"
if [ "${1:-}" = "--gitdeps" ]; then
  TMP="$(mktemp -d)"
  python3 "$ROOT/tools/ue-gitdeps-fetch.py" "$2" "$TMP" Templates/TP_ThirdPerson/Content/ThirdPerson/ Templates/TP_ThirdPerson/Content/__ExternalActors__/ThirdPerson/ Templates/TP_ThirdPerson/Content/__ExternalObjects__/ThirdPerson/ Templates/TemplateResources/High/Characters/Content/ Templates/TemplateResources/High/LevelPrototyping/Content/ Templates/TemplateResources/High/Input/Content/
  SRC="$TMP"
else
  SRC="${1:?engine root}"
fi
cp -r "$SRC/Templates/TP_ThirdPerson/Content/." "$DST/"
for P in Characters LevelPrototyping Input; do mkdir -p "$DST/$P"; cp -r "$SRC/Templates/TemplateResources/High/$P/Content/." "$DST/$P/"; done
du -sh "$DST"
