#!/usr/bin/env bash
# Node-in-the-loop (phase 9f): a live node + two headless UE dedicated servers (one cell each) + one headless UE
# client, all from the Epic dev-slim image of `<ver>`; the entity walks across the cell border, the client must
# stay connected (in-place rehome) and keep its actor. Needs `tools/ue-build.sh <ver>` green first (the plugin
# binaries come from target/ue/<ver>/Package) and the dev Postgres (`docker start musubi-pg`).
#   tools/nitl.sh 5.3.2 [seconds]
#   NITL_PROJECT=target/ue/demo-5.8.4/MusubiThirdPerson NITL_MAP=/Game/ThirdPerson/Lvl_ThirdPerson NITL_SPAWN_Z=300 \
#     tools/nitl.sh 5.8.4 [seconds]      # the same run on the Third Person demo (after tools/demo-build.sh 5.8.4)
#   NITL_LOGCMDS_EXTRA=", LogWebSockets VeryVerbose" tools/nitl.sh 5.8.4   # extra -LogCmds for the UE processes
set -euo pipefail
VER="${1:?engine version, e.g. 5.3.2}"; DUR="${2:-90}"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
IMG="${UE_IMAGE:-ghcr.io/epicgames/unreal-engine:dev-slim-$VER}"
PKG="$ROOT/target/ue/$VER/Package"; OUT="$ROOT/target/ue/nitl-$VER"; PROJ="$OUT/HostProject"
API="${MUSUBI_API:-http://127.0.0.1:7360}"; KEY="${MUSUBI_SERVER_KEY:-defaultkey}"; WORLD="nitl$(date +%s)"
DB="${MUSUBI_TEST_DATABASE_URL:-postgres://musubi:musubi@localhost:5433/musubi}"
# NITL_CHAOS=1 (phase 9g-7): node netsim on the client sessions (latency/jitter/loss), a second client on its own
# entity owned by s2, s2 killed while the handover is in flight, s2b takes the cell over.
CHAOS="${NITL_CHAOS:-0}"; NAMES="nitl-s1 nitl-s2 nitl-c"; [ "$CHAOS" = 1 ] && NAMES="nitl-s1 nitl-s2 nitl-c nitl-c2 nitl-s2b"
# NITL_PROJECT=<dir of a built project incl. Plugins/Musubi> + NITL_MAP=<map>: run the same scenario on another project
# (the Third Person demo, phase 9g-9: `tools/demo-build.sh <ver>` then NITL_PROJECT=target/ue/demo-<ver>/MusubiThirdPerson).
MAP="${NITL_MAP:-/Engine/Maps/Entry}"; UPROJ=HostProject.uproject
rm -rf "$OUT"; mkdir -p "$OUT/home"
if [ -n "${NITL_PROJECT:-}" ]; then
  # the game module's RPATH to Plugins/Musubi is anchored at the build path, so every instance is mounted at /out/<name>
  cp -r "$NITL_PROJECT" "$PROJ"; rm -rf "$PROJ/Saved" "$PROJ/Intermediate" "$PROJ/DerivedDataCache"
  UPROJ=$(cd "$PROJ" && ls *.uproject | head -1); PNAME=$(basename "$NITL_PROJECT")
else
  [ -f "$PKG/Musubi.uplugin" ] || { echo "no plugin build in $PKG — run tools/ue-build.sh $VER"; exit 2; }
  cp -r "$ROOT/sdk/unreal/tools/nitl/HostProject" "$PROJ"
  mkdir -p "$PROJ/Plugins"; cp -r "$PKG" "$PROJ/Plugins/Musubi"
  cp -r "$ROOT/sdk/unreal/Musubi/Config" "$PROJ/Plugins/Musubi/"  # BuildPlugin output has no Config/
  sed -i "s/\"EngineAssociation\": \"\"/\"EngineAssociation\": \"${VER%.*}\"/" "$PROJ/HostProject.uproject"
fi

cleanup() { set +e; curl -s http://127.0.0.1:7360/metrics > "$OUT/metrics.txt" 2>/dev/null; for N in $NAMES; do docker logs "$N" > "$OUT/$N.log" 2>&1; done; for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; docker rm -f nitl-s1 nitl-s2 nitl-c >/dev/null 2>&1; }
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
X0=$((CELL - 40)); WALK=${NITL_WALK:-60}; [ "$CHAOS" = 1 ] && X0=$((CELL / 4)) # chaos: a longer walk to the border so s2 can be killed just before it

ue() { # name, extra args...
  local N="$1"; shift
  # every instance gets its own project copy + HOME: UE processes sharing Saved/DDC/shader autogen race at boot
  docker rm -f "$N" >/dev/null 2>&1 || true # a leftover from an aborted run must not block this one
  cp -r "$PROJ" "$OUT/proj-$N"; mkdir -p "$OUT/home-$N"
  local UPATH="/out/proj-$N/$UPROJ" MOUNT=(); if [ -n "${NITL_PROJECT:-}" ]; then MOUNT=(-v "$OUT/proj-$N:/out/$PNAME"); UPATH="/out/$PNAME/$UPROJ"; fi
  docker run -d --name "$N" --network host -u "$(id -u):$(id -g)" -e HOME="/out/home-$N" -v "$OUT:/out" "${MOUNT[@]}" "$IMG" \
    /home/ue4/UnrealEngine/Engine/Binaries/Linux/UnrealEditor "$UPATH" "$MAP" \
    -unattended -nosplash -nullrhi -NoSound -stdout -FullStdOutLogOutput -log -LogCmds="LogMusubiServer VeryVerbose, LogMusubiNet Verbose, LogMusubiMesh Verbose, LogNet Verbose${NITL_LOGCMDS_EXTRA:-}" \
    -MusubiApi="$API" -MusubiKey="$KEY" -MusubiWorld="$WORLD" "$@" >/dev/null
}
echo "== servers"
S2SPAWN=""; if [ "$CHAOS" = 1 ]; then
  S2SPAWN="-MusubiNitlSpawn=$((CELL + CELL / 2)),100${NITL_SPAWN_Z:+,$NITL_SPAWN_Z}"
  curl -fsS -X PUT "$API/v1/admin/dev/netsim" -H "x-musubi-server-key: $KEY" -H 'content-type: application/json' -d '{"latency_ms":60,"jitter_ms":20,"loss_pct":2}' > "$OUT/netsim.json" 2>&1 || curl -fsS -X POST "$API/v1/admin/dev/netsim" -H "x-musubi-server-key: $KEY" -H 'content-type: application/json' -d '{"latency_ms":60,"jitter_ms":20,"loss_pct":2}' > "$OUT/netsim.json" 2>&1
  echo "netsim: $(head -c 300 "$OUT/netsim.json")"
fi
SZ=""; [ -n "${NITL_SPAWN_Z:-}" ] && SZ=",$NITL_SPAWN_Z" # a map with a floor (the demo): spawn height, the Entry map has none
ue nitl-s1 -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="0,0" -MusubiNitlSpawn="$X0,100$SZ" -MusubiNitlWalk="$WALK"
ue nitl-s2 -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="1,0" -MusubiNitlWalk="$WALK" $S2SPAWN
ENTITY=""
for i in $(seq 1 180); do
  ENTITY=$(docker logs nitl-s1 2>&1 | sed -n 's/.*nitl: spawned \([0-9a-f-]*\).*/\1/p' | head -1); [ -n "$ENTITY" ] && break; sleep 1
done
curl -fsS "$API/v1/server/worlds/$WORLD" -H "x-musubi-server-key: $KEY" > "$OUT/world-after.json" 2>/dev/null; echo "world state: $(head -c 600 "$OUT/world-after.json")"
[ -n "$ENTITY" ] || { echo "s1 never spawned the entity"; docker logs nitl-s1 2>&1 | grep -i "musubi\|error\|warning: " | tail -40; exit 1; }
echo "entity=$ENTITY"
ENTITY2=""; if [ "$CHAOS" = 1 ]; then
  for i in $(seq 1 120); do ENTITY2=$(docker logs nitl-s2 2>&1 | sed -n 's/.*nitl: spawned \([0-9a-f-]*\).*/\1/p' | head -1); [ -n "$ENTITY2" ] && break; sleep 1; done
  [ -n "$ENTITY2" ] || { echo "s2 never spawned the second entity"; exit 1; }; echo "entity2=$ENTITY2"
fi
echo "== client"
ue nitl-c -game -MusubiNitl=client -MusubiEntity="$ENTITY" -MusubiNitlWalk="$WALK"
if [ "$CHAOS" = 1 ]; then
  ue nitl-c2 -game -MusubiNitl=client -MusubiEntity="$ENTITY2" -MusubiNitlWalk=0
  # kill s2 when the walking pawn is 1-2 s from the border: its lease (2 s) is still valid, so the node starts the
  # handover towards a dead server; no ack must come back and the handover must be cancelled, not lost
  T0=$(date +%s); KILLED=""
  while [ $(( $(date +%s) - T0 )) -lt 100 ]; do
    PX=$(docker logs --since 3s nitl-s1 2>&1 | grep "nitl: server pawn .* id=$ENTITY " | tail -1 | sed -n 's/.* at \([-0-9]*\),.*/\1/p')
    if [ -n "$PX" ] && [ "$PX" -gt $((CELL - WALK * 3 / 2)) ]; then docker kill nitl-s2 >/dev/null 2>&1; KILLED=$(( $(date +%s) - T0 )); echo "== killed s2 at +${KILLED}s, pawn x=$PX (border $CELL)"; break; fi
    sleep 0.3
  done
  [ -n "$KILLED" ] || echo "== WARNING: never reached the kill point"
  sleep 10; echo "== s2b"; # the pawn kept walking while s2 was dead: s2b takes the whole row so the retried handover has an owner wherever it is
  ue nitl-s2b -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="1,0;2,0;3,0;4,0;5,0;6,0;7,0;8,0;9,0;10,0;11,0;12,0" -MusubiNitlWalk="$WALK"
  sleep $(( DUR - ${KILLED:-0} - 10 > 20 ? DUR - ${KILLED:-0} - 10 : 20 ))
  curl -fsS "$API/v1/admin/dev/netsim" -H "x-musubi-server-key: $KEY" > "$OUT/netsim-after.json" 2>/dev/null
  curl -fsS "$API/v1/server/worlds/$WORLD" -H "x-musubi-server-key: $KEY" > "$OUT/world-end.json" 2>/dev/null
  curl -fsS "$API/v1/server/worlds/$WORLD/entities/$ENTITY" -H "x-musubi-server-key: $KEY" > "$OUT/entity-end.json" 2>/dev/null
else
  sleep "$DUR"
fi
for N in $NAMES; do docker logs "$N" > "$OUT/$N.log" 2>&1; done
echo "== verdict"
set +e # every check below is a grep pipeline that may legitimately find nothing
ok=1
S2LOG=nitl-s2; PAWNS=1; [ "$CHAOS" = 1 ] && { S2LOG=nitl-s2b; PAWNS=2; }
chk() { if grep -q -- "$2" "$OUT/$1.log"; then echo "PASS $1: $2"; else echo "FAIL $1: $2"; ok=0; fi; }
chk nitl-s1 "nitl: handover $ENTITY completed"
chk $S2LOG "nitl: handover $ENTITY request ack"
chk nitl-c  "nitl: travelling to musubi://"
chk nitl-c  "Welcomed by server"
chk nitl-c  "rehoming in place"
chk nitl-c  "rehomed in place"
LASTP=$(grep "client pawns=" "$OUT/nitl-c.log" | tail -1); case "$LASTP" in *"pawns=$PAWNS "*) echo "PASS nitl-c: $PAWNS pawn(s) at the end";; *) echo "FAIL nitl-c: $PAWNS pawn(s) at the end ($LASTP)"; ok=0;; esac
LASTX=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* at \([-0-9]*\),.*/\1/p'); if [ -n "$LASTX" ] && [ "$LASTX" -gt 300 ]; then echo "PASS nitl-c: pawn kept moving after rehome (x=$LASTX)"; else echo "FAIL nitl-c: pawn kept moving after rehome (x=$LASTX)"; ok=0; fi
chk nitl-c  "rehome: swapped"
# push-model property on the same actor: the classic driver must still see MARK_PROPERTY_DIRTY on an actor the Iris export registered
LASTS=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* steps=\([0-9]*\).*/\1/p'); LASTPUSH=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* push=\([0-9]*\).*/\1/p')
if [ -n "${NITL_PROJECT:-}" ] && [ "${LASTS:--1}" = "-1" ]; then echo "SKIP nitl-c: push-model property (the project's pawn is not the NITL pawn)"; else
if [ -n "$LASTPUSH" ] && [ -n "$LASTS" ] && [ "$LASTPUSH" -gt 0 ] && [ $((LASTS - LASTPUSH)) -lt 120 ]; then echo "PASS nitl-c: push-model property replicated (steps=$LASTS push=$LASTPUSH)"; else echo "FAIL nitl-c: push-model property replicated (steps=$LASTS push=$LASTPUSH)"; ok=0; fi
fi
chk nitl-c  "nitl: identified"
# the player: possessed its entity's pawn before the rehome, possesses the new owner's copy after it, PlayerState name survived
REH=$(grep -n "rehomed in place" "$OUT/nitl-c.log" | head -1 | cut -d: -f1); REH=${REH:-0}
BEFORE=$(grep -n "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | awk -F: -v n="$REH" '$1<n' | grep "possessed=1" | tail -1)
AFTER=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1)
if [ -n "$BEFORE" ]; then echo "PASS nitl-c: player possessed its pawn before the rehome"; else echo "FAIL nitl-c: player possessed its pawn before the rehome"; ok=0; fi
case "$AFTER" in *"possessed=1"*) echo "PASS nitl-c: player possesses the new owner's pawn after the rehome";; *) echo "FAIL nitl-c: player possesses the new owner's pawn after the rehome ($AFTER)"; ok=0;; esac
N1=$(echo "$BEFORE" | sed -n 's/.* name=\([^ ]*\).*/\1/p'); N2=$(echo "$AFTER" | sed -n 's/.* name=\([^ ]*\).*/\1/p')
if [ -n "$N1" ] && [ "$N1" != "-" ] && [ "$N1" = "$N2" ]; then echo "PASS nitl-c: PlayerState name survived the rehome ($N1)"; else echo "FAIL nitl-c: PlayerState name survived the rehome (before=$N1 after=$N2)"; ok=0; fi
# the stable handle: taken from the first pawn, resolves to the live possessed pawn after the swap, exactly one actor change seen
H=$(grep "nitl: handle " "$OUT/nitl-c.log" | tail -1 | sed 's/.*nitl: handle //')
case "$H" in *"changes=1 live=1"|*"changes=$([ "$CHAOS" = 1 ] && echo 2 || echo 1) live=1") echo "PASS nitl-c: entity handle resolves to the live pawn after the rehome ($H)";; *) echo "FAIL nitl-c: entity handle resolves to the live pawn after the rehome ($H)"; ok=0;; esac
# replicated components in the binary iris state: the ghost on s2 saw the inventory before owning, the new owner carried on from it
EXPC=$(grep -o "first binary iris state exported ([0-9]* component" "$OUT/nitl-s1.log" | head -1 | grep -o "[0-9]*")
if [ "${EXPC:-0}" -ge 1 ]; then echo "PASS nitl-s1: replicated components exported in the iris state ($EXPC)"; else echo "FAIL nitl-s1: replicated components exported in the iris state (${EXPC:-0})"; ok=0; fi
GINV=$(grep "nitl: server pawn .* id=$ENTITY owned=0 " "$OUT/$S2LOG.log" | grep -o "inv=[0-9]* gold=[0-9]*" | tail -1)
GHOW="ghost received the inventory component via iris"
if [ -z "$GINV" ]; then
  # no ghost phase: the entity arrived already owned (handover retried into a replacement server) — the owned spawn must restore it from the node
  GINV=$(grep "nitl: server pawn .* id=$ENTITY owned=1 " "$OUT/$S2LOG.log" | grep -o "inv=[0-9]* gold=[0-9]*" | head -1); GHOW="owned spawn restored the inventory component from the node's iris state"
fi
case "$GINV" in inv=0*|"") echo "FAIL nitl-s2: $GHOW ($GINV)"; ok=0;; *) echo "PASS nitl-s2: $GHOW ($GINV)";; esac
HO=$(grep -n "nitl: handover .* completed" "$OUT/nitl-s1.log" | head -1 | cut -d: -f1); HO=${HO:-0}
S1INV=$(grep -n "nitl: server pawn .* id=$ENTITY owned=1 " "$OUT/nitl-s1.log" | awk -F: -v n="$HO" '$1<n' | grep -o "inv=[0-9]* gold=[0-9]*" | tail -1)
S2INV=$(grep "nitl: server pawn .* id=$ENTITY owned=1 " "$OUT/$S2LOG.log" | grep -o "inv=[0-9]* gold=[0-9]*" | head -1)
A1=$(echo "$S1INV" | grep -o "inv=[0-9]*" | cut -d= -f2); A2=$(echo "$S2INV" | grep -o "inv=[0-9]*" | cut -d= -f2)
G1=$(echo "$S1INV" | grep -o "gold=[0-9]*" | cut -d= -f2); G2=$(echo "$S2INV" | grep -o "gold=[0-9]*" | cut -d= -f2)
if [ -n "$A1" ] && [ -n "$A2" ] && [ "$A1" -ge 1 ] && [ "$A2" -ge "$A1" ] && [ "${G2:-0}" -ge "${G1:-1}" ]; then echo "PASS nitl-s: inventory carried across the handover without reset (s1 $S1INV -> s2 $S2INV)"; else echo "FAIL nitl-s: inventory carried across the handover without reset (s1 $S1INV -> s2 $S2INV)"; ok=0; fi
# member-level deltas: keyframes are rare and big, deltas frequent and small (iris stats line on the owner)
ST=$(grep -o "iris stats keyframes=[0-9]* ([0-9]* B, avg [0-9]*) deltas=[0-9]* ([0-9]* B, avg [0-9]*)" "$OUT/nitl-s1.log" | tail -1)
KN=$(echo "$ST" | sed -n 's/.*keyframes=\([0-9]*\).*/\1/p'); KA=$(echo "$ST" | sed -n 's/.*keyframes=[0-9]* ([0-9]* B, avg \([0-9]*\)).*/\1/p'); DN=$(echo "$ST" | sed -n 's/.*deltas=\([0-9]*\).*/\1/p'); DA=$(echo "$ST" | sed -n 's/.*deltas=[0-9]* ([0-9]* B, avg \([0-9]*\)).*/\1/p')
DMIN=$KN; [ -n "${NITL_PROJECT:-}" ] && DMIN=0 # a game pawn changes rarely (the demo: gold every 2 s): the ratio only holds for the NITL pawn
if [ -n "$KN" ] && [ -n "$DN" ] && [ "$DN" -gt "$DMIN" ] && [ "${DA:-999999}" -lt "${KA:-0}" ]; then echo "PASS nitl-s1: member-level deltas (keyframes=$KN avg ${KA}B, deltas=$DN avg ${DA}B)"; else echo "FAIL nitl-s1: member-level deltas ($ST)"; ok=0; fi
CINV=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1 | grep -o "inv=[0-9]*" | cut -d= -f2)
if [ -n "$CINV" ] && [ -n "$A1" ] && [ "$CINV" -ge "$A1" ]; then echo "PASS nitl-c: client sees the inventory (FastArray) from the new owner (inv=$CINV)"; else echo "FAIL nitl-c: client sees the inventory (FastArray) from the new owner (inv=$CINV, s1 had $A1)"; ok=0; fi
if grep -q "already has a push id" "$OUT/nitl-s1.log" "$OUT/$S2LOG.log"; then echo "FAIL nitl-s: no push id conflict between the Iris export and the GameNetDriver"; ok=0; else echo "PASS nitl-s: no push id conflict between the Iris export and the GameNetDriver"; fi
if [ "$CHAOS" = 1 ]; then
  chk nitl-s1 "nitl: handover $ENTITY cancelled"
  chk nitl-c2 "Welcomed by server"
  chk nitl-c2 "rehomed in place"
  TR=$(grep -c "nitl: travelling to musubi://" "$OUT/nitl-c.log"); if [ "$TR" = 1 ]; then echo "PASS nitl-c: never fell back to travel under loss + owner death"; else echo "FAIL nitl-c: never fell back to travel (travels=$TR)"; ok=0; fi
  F1=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c2.log" | head -1 | sed -n 's/.* steps=\([0-9]*\).*/\1/p'); L1=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c2.log" | tail -1 | sed -n 's/.* steps=\([0-9]*\).*/\1/p')
  if [ -n "$F1" ] && [ -n "$L1" ] && [ "$L1" -gt "$F1" ]; then echo "PASS nitl-c2: the other client sees the walking pawn through both owners (steps $F1 -> $L1)"; else echo "FAIL nitl-c2: the other client sees the walking pawn (steps $F1 -> $L1)"; ok=0; fi
  DROP=$(python3 -c "import json,sys;d=json.load(open('$OUT/netsim-after.json'));print(sum(v for k,v in (d.get('counters') or d).items() if isinstance(v,(int,float)) and 'drop' in k))" 2>/dev/null)
  if [ -n "$DROP" ] && [ "${DROP%.*}" -gt 0 ]; then echo "PASS node: netsim dropped packets on the client sessions ($DROP)"; else echo "FAIL node: netsim dropped packets ($(head -c 200 "$OUT/netsim-after.json" 2>/dev/null))"; ok=0; fi
fi
grep -h "nitl: client pawn" "$OUT/nitl-c.log" | tail -3
if [ -n "${NITL_PROJECT:-}" ]; then # the project's own Blueprint logic ran on every side (demo: PrintString from the authored graph)
  chk nitl-s1 "musubi-demo owned entity $ENTITY"
  chk $S2LOG "musubi-demo owned entity $ENTITY"
  chk nitl-c "musubi-demo inventory items="
fi
[ "$ok" = 1 ] && echo "NITL PASS ($VER)" || { echo "NITL FAIL ($VER) — logs in $OUT"; exit 1; }
