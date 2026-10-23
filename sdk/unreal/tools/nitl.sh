#!/usr/bin/env bash
# Node-in-the-loop (phase 9f): a live node + two headless UE dedicated servers (one cell each) + one headless UE
# client, all from the Epic dev-slim image of `<ver>`; the entity walks across the cell border, the client must
# stay connected (in-place rehome) and keep its actor. Needs `tools/ue-build.sh <ver>` green first (the plugin
# binaries come from target/ue/<ver>/Package) and the dev Postgres (`docker start musubi-pg`).
#   tools/nitl.sh 5.3.2 [seconds]
set -euo pipefail
VER="${1:?engine version, e.g. 5.3.2}"; DUR="${2:-90}"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
IMG="${UE_IMAGE:-ghcr.io/epicgames/unreal-engine:dev-slim-$VER}"
PKG="$ROOT/target/ue/$VER/Package"; OUT="$ROOT/target/ue/nitl-$VER"; PROJ="$OUT/HostProject"
API="${MUSUBI_API:-http://127.0.0.1:7360}"; KEY="${MUSUBI_SERVER_KEY:-defaultkey}"; WORLD="nitl$(date +%s)"
DB="${MUSUBI_TEST_DATABASE_URL:-postgres://musubi:musubi@localhost:5433/musubi}"
[ -f "$PKG/Musubi.uplugin" ] || { echo "no plugin build in $PKG — run tools/ue-build.sh $VER"; exit 2; }
rm -rf "$OUT"; mkdir -p "$OUT/home"; cp -r "$ROOT/sdk/unreal/tools/nitl/HostProject" "$PROJ"
mkdir -p "$PROJ/Plugins"; cp -r "$PKG" "$PROJ/Plugins/Musubi"
cp -r "$ROOT/sdk/unreal/Musubi/Config" "$PROJ/Plugins/Musubi/"  # BuildPlugin output has no Config/
sed -i "s/\"EngineAssociation\": \"\"/\"EngineAssociation\": \"${VER%.*}\"/" "$PROJ/HostProject.uproject"

cleanup() { set +e; curl -s http://127.0.0.1:7360/metrics > "$OUT/metrics.txt" 2>/dev/null; for N in nitl-s1 nitl-s2 nitl-c; do docker logs "$N" > "$OUT/$N.log" 2>&1; done; for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; docker rm -f nitl-s1 nitl-s2 nitl-c >/dev/null 2>&1; }
trap cleanup EXIT
PIDS=()
if ! curl -fsS "$API/healthz" >/dev/null 2>&1; then
  echo "== node"; RUST_LOG="${NITL_RUST_LOG:-info,musubi_server::realtime=debug}" "$ROOT/target/debug/musubi" dev --database-url "$DB" --listen "${API#http://}" > "$OUT/node.log" 2>&1 & PIDS+=($!)
  for i in $(seq 1 60); do curl -fsS "$API/healthz" >/dev/null 2>&1 && break; sleep 1; done
fi
curl -fsS "$API/healthz" >/dev/null || { echo "node not up"; tail -20 "$OUT/node.log"; exit 1; }
echo "== world $WORLD"
curl -fsS -X POST "$API/v1/server/worlds" -H "x-musubi-server-key: $KEY" -H 'content-type: application/json' -d "{\"name\":\"$WORLD\"}" > "$OUT/world.json"
CELL=$(python3 -c "import json,sys;d=json.load(open('$OUT/world.json'));print(int(d.get('config',d).get('cell_size',1000)))")
echo "cell_size=$CELL"
X0=$((CELL - 40)); WALK=${NITL_WALK:-60}

ue() { # name, extra args...
  local N="$1"; shift
  # every instance gets its own project copy + HOME: UE processes sharing Saved/DDC/shader autogen race at boot
  cp -r "$PROJ" "$OUT/proj-$N"; mkdir -p "$OUT/home-$N"
  docker run -d --name "$N" --network host -u "$(id -u):$(id -g)" -e HOME="/out/home-$N" -v "$OUT:/out" "$IMG" \
    /home/ue4/UnrealEngine/Engine/Binaries/Linux/UnrealEditor "/out/proj-$N/HostProject.uproject" /Engine/Maps/Entry \
    -unattended -nosplash -nullrhi -NoSound -stdout -FullStdOutLogOutput -log -LogCmds="LogMusubiServer VeryVerbose, LogMusubiNet Verbose, LogMusubiMesh Verbose, LogNet Verbose" \
    -MusubiApi="$API" -MusubiKey="$KEY" -MusubiWorld="$WORLD" "$@" >/dev/null
}
echo "== servers"
ue nitl-s1 -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="0,0" -MusubiNitlSpawn="$X0,100" -MusubiNitlWalk="$WALK"
ue nitl-s2 -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="1,0" -MusubiNitlWalk="$WALK"
ENTITY=""
for i in $(seq 1 180); do
  ENTITY=$(docker logs nitl-s1 2>&1 | sed -n 's/.*nitl: spawned \([0-9a-f-]*\).*/\1/p' | head -1); [ -n "$ENTITY" ] && break; sleep 1
done
curl -fsS "$API/v1/server/worlds/$WORLD" -H "x-musubi-server-key: $KEY" > "$OUT/world-after.json" 2>/dev/null; echo "world state: $(head -c 600 "$OUT/world-after.json")"
[ -n "$ENTITY" ] || { echo "s1 never spawned the entity"; docker logs nitl-s1 2>&1 | grep -i "musubi\|error\|warning: " | tail -40; exit 1; }
echo "entity=$ENTITY"
echo "== client"
ue nitl-c -game -MusubiNitl=client -MusubiEntity="$ENTITY" -MusubiNitlWalk="$WALK"
sleep "$DUR"
for N in nitl-s1 nitl-s2 nitl-c; do docker logs "$N" > "$OUT/$N.log" 2>&1; done
echo "== verdict"
ok=1
chk() { if grep -q -- "$2" "$OUT/$1.log"; then echo "PASS $1: $2"; else echo "FAIL $1: $2"; ok=0; fi; }
chk nitl-s1 "nitl: handover $ENTITY completed"
chk nitl-s2 "nitl: handover $ENTITY request ack"
chk nitl-c  "nitl: travelling to musubi://"
chk nitl-c  "Welcomed by server"
chk nitl-c  "rehoming in place"
chk nitl-c  "rehomed in place"
LASTP=$(grep "client pawns=" "$OUT/nitl-c.log" | tail -1); case "$LASTP" in *"pawns=1 "*) echo "PASS nitl-c: one pawn at the end";; *) echo "FAIL nitl-c: one pawn at the end ($LASTP)"; ok=0;; esac
LASTX=$(grep "nitl: client pawn " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* at \([-0-9]*\),.*/\1/p'); if [ -n "$LASTX" ] && [ "$LASTX" -gt 300 ]; then echo "PASS nitl-c: pawn kept moving after rehome (x=$LASTX)"; else echo "FAIL nitl-c: pawn kept moving after rehome (x=$LASTX)"; ok=0; fi
chk nitl-c  "rehome: swapped"
# push-model property on the same actor: the classic driver must still see MARK_PROPERTY_DIRTY on an actor the Iris export registered
LASTS=$(grep "nitl: client pawn " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* steps=\([0-9]*\).*/\1/p'); LASTPUSH=$(grep "nitl: client pawn " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* push=\([0-9]*\).*/\1/p')
if [ -n "$LASTPUSH" ] && [ -n "$LASTS" ] && [ "$LASTPUSH" -gt 0 ] && [ $((LASTS - LASTPUSH)) -lt 120 ]; then echo "PASS nitl-c: push-model property replicated (steps=$LASTS push=$LASTPUSH)"; else echo "FAIL nitl-c: push-model property replicated (steps=$LASTS push=$LASTPUSH)"; ok=0; fi
chk nitl-c  "nitl: identified"
# the player: possessed its entity's pawn before the rehome, possesses the new owner's copy after it, PlayerState name survived
REH=$(grep -n "rehomed in place" "$OUT/nitl-c.log" | head -1 | cut -d: -f1); REH=${REH:-0}
BEFORE=$(grep -n "nitl: client pawn " "$OUT/nitl-c.log" | awk -F: -v n="$REH" '$1<n' | grep "possessed=1" | tail -1)
AFTER=$(grep "nitl: client pawn " "$OUT/nitl-c.log" | tail -1)
if [ -n "$BEFORE" ]; then echo "PASS nitl-c: player possessed its pawn before the rehome"; else echo "FAIL nitl-c: player possessed its pawn before the rehome"; ok=0; fi
case "$AFTER" in *"possessed=1"*) echo "PASS nitl-c: player possesses the new owner's pawn after the rehome";; *) echo "FAIL nitl-c: player possesses the new owner's pawn after the rehome ($AFTER)"; ok=0;; esac
N1=$(echo "$BEFORE" | sed -n 's/.* name=\(.*\)$/\1/p'); N2=$(echo "$AFTER" | sed -n 's/.* name=\(.*\)$/\1/p')
if [ -n "$N1" ] && [ "$N1" != "-" ] && [ "$N1" = "$N2" ]; then echo "PASS nitl-c: PlayerState name survived the rehome ($N1)"; else echo "FAIL nitl-c: PlayerState name survived the rehome (before=$N1 after=$N2)"; ok=0; fi
# the stable handle: taken from the first pawn, resolves to the live possessed pawn after the swap, exactly one actor change seen
H=$(grep "nitl: handle " "$OUT/nitl-c.log" | tail -1 | sed 's/.*nitl: handle //')
case "$H" in *"changes=1 live=1") echo "PASS nitl-c: entity handle resolves to the live pawn after the rehome ($H)";; *) echo "FAIL nitl-c: entity handle resolves to the live pawn after the rehome ($H)"; ok=0;; esac
if grep -q "already has a push id" "$OUT/nitl-s1.log" "$OUT/nitl-s2.log"; then echo "FAIL nitl-s: no push id conflict between the Iris export and the GameNetDriver"; ok=0; else echo "PASS nitl-s: no push id conflict between the Iris export and the GameNetDriver"; fi
grep -h "nitl: client pawn" "$OUT/nitl-c.log" | tail -3
[ "$ok" = 1 ] && echo "NITL PASS ($VER)" || { echo "NITL FAIL ($VER) — logs in $OUT"; exit 1; }
