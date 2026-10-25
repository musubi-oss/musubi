#!/usr/bin/env bash
# Node-in-the-loop (phase 9f): a live node + two headless UE dedicated servers (one cell each) + one headless UE
# client, all from the Epic dev-slim image of `<ver>`; the entity walks across the cell border, the client must
# stay connected (in-place rehome) and keep its actor. Needs `tools/ue-build.sh <ver>` green first (the plugin
# binaries come from target/ue/<ver>/Package) and the dev Postgres (`docker start musubi-pg`).
#   tools/nitl.sh 5.3.2 [seconds]
#   NITL_PROJECT=target/ue/demo-5.8.4/MusubiThirdPerson NITL_MAP=/Game/ThirdPerson/Lvl_ThirdPerson NITL_SPAWN_Z=300 \
#     tools/nitl.sh 5.8.4 [seconds]      # the same run on the Third Person demo (after tools/demo-build.sh 5.8.4)
#   NITL_LOGCMDS_EXTRA=", LogWebSockets VeryVerbose" tools/nitl.sh 5.8.4   # extra -LogCmds for the UE processes
#   NITL_IRIS=1 NITL_JOIN=1 tools/nitl.sh 5.7.4   # relay driver on Iris: same pawn/PC/PS/subobject across the rehome (9.2)
set -euo pipefail
VER="${1:?engine version, e.g. 5.3.2}"; DUR="${2:-90}"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
IMG="${UE_IMAGE:-ghcr.io/epicgames/unreal-engine:dev-slim-$VER}"
PKG="$ROOT/target/ue/$VER/Package"; OUT="$ROOT/target/ue/nitl-$VER"; PROJ="$OUT/HostProject"
API="${MUSUBI_API:-http://127.0.0.1:7360}"; KEY="${MUSUBI_SERVER_KEY:-defaultkey}"; WORLD="nitl$(date +%s)"
DB="${MUSUBI_TEST_DATABASE_URL:-postgres://musubi:musubi@localhost:5433/musubi}"
# NITL_CHAOS=1 (phase 9g-7): node netsim on the client sessions (latency/jitter/loss), a second client on its own
# entity owned by s2, s2 killed while the handover is in flight, s2b takes the cell over.
# NITL_JOIN=1: the world carries a player_spawn policy and s1 spawns nothing; the client travels to musubi://<world>:0
# without an entity and the node spawns its avatar on world_join (owner = the client's account); s1 walks it.
JOIN="${NITL_JOIN:-0}"
# NITL_ENCRYPT=1: AES-256-GCM on the relayed engine connections through the standard PacketHandler chain (-MusubiNitlEncrypt
# keys both ends; the client travels with ?EncryptionToken=, every owner acks it, the rehome re-keys with the new owner).
ENCRYPT="${NITL_ENCRYPT:-0}"; ENCARG=""; [ "$ENCRYPT" = 1 ] && ENCARG="-MusubiNitlEncrypt"
# NITL_IRIS=1: the relay GameNetDriver (client <-> owning server) runs Iris replication on every UE process
# (bCanUseIris=true is in the HostProject's NetDriverDefinitions; the cvar decides at driver creation).
IRIS="${NITL_IRIS:-0}"; IRISARG=""; [ "$IRIS" = 1 ] && IRISARG="-ini:Engine:[SystemSettings]:net.Iris.UseIrisReplication=1 -ini:Engine:[SystemSettings]:net.SubObjects.DefaultUseSubObjectReplicationList=1"
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

cleanup() { set +e; curl -s http://127.0.0.1:7360/metrics > "$OUT/metrics.txt" 2>/dev/null; for N in $NAMES; do docker logs "$N" > "$OUT/$N.log" 2>&1; done; for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; wait "${PIDS[@]:-}" 2>/dev/null; docker rm -f $NAMES >/dev/null 2>&1; } # wait: the node we started must be gone before a back-to-back run probes healthz
trap cleanup EXIT
PIDS=()
if ! curl -fsS "$API/healthz" >/dev/null 2>&1; then
  echo "== node"; RUST_LOG="${NITL_RUST_LOG:-info,musubi_server::realtime=debug}" "$ROOT/target/debug/musubi" dev --database-url "$DB" --listen "${API#http://}" > "$OUT/node.log" 2>&1 & PIDS+=($!)
  for i in $(seq 1 60); do curl -fsS "$API/healthz" >/dev/null 2>&1 && break; sleep 1; done
fi
curl -fsS "$API/healthz" >/dev/null || { echo "node not up"; tail -20 "$OUT/node.log"; exit 1; }
echo "== world $WORLD"
WBODY="{\"name\":\"$WORLD\"}"
if [ "$JOIN" = 1 ]; then
  # default cell size 256 (see WorldConfig::default); the avatar appears 40 units before the border of cell 0,0
  JX=$((${NITL_CELL:-256} - 40)); JZ="${NITL_SPAWN_Z:-0}"
  WBODY="{\"name\":\"$WORLD\",\"config\":{\"player_spawn\":{\"components\":{\"z\":$JZ},\"points\":[{\"x\":$JX,\"y\":100,\"z\":0}],\"persistent\":false}}}"
fi
curl -fsS -X POST "$API/v1/server/worlds" -H "x-musubi-server-key: $KEY" -H 'content-type: application/json' -d "$WBODY" > "$OUT/world.json"
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
    -MusubiApi="$API" -MusubiKey="$KEY" -MusubiWorld="$WORLD" $ENCARG $IRISARG "$@" >/dev/null
}
# NITL_WP=1: a World Partition map built headless by the plugin's commandlet (grid = the world's cell size, a marker actor
# per cell along x); the servers stream it by their leases, the client by its controller — one map, no travel on rehome.
WP="${NITL_WP:-0}"; WPCLIENT=""
if [ "$WP" = 1 ] && [ -z "${NITL_PROJECT:-}" ]; then
  echo "== wp map"
  docker run --rm --network none -u "$(id -u):$(id -g)" -e HOME="/out/home" -v "$OUT:/out" "$IMG" \
    /home/ue4/UnrealEngine/Engine/Binaries/Linux/UnrealEditor-Cmd "/out/HostProject/$UPROJ" -run=MusubiNitlWpMap -Cell="$CELL" -Markers=12 \
    -unattended -nosplash -nullrhi -NoSound -stdout -FullStdOutLogOutput > "$OUT/wpmap.log" 2>&1 || { echo "wp map commandlet failed"; grep -i "error\|wpmap" "$OUT/wpmap.log" | tail -20; exit 1; }
  [ -f "$PROJ/Content/NitlWP.umap" ] || { echo "wp map not written"; grep -i "wpmap\|error" "$OUT/wpmap.log" | tail -20; exit 1; }
  echo "wp map: $(grep -o 'wpmap: .*' "$OUT/wpmap.log" | tail -1)"
  MAP=/Game/NitlWP
  # An uncooked `UnrealEditor -server` has no WP cell packages on disk and kicks a client that reports a WP cell visible
  # which this server has not streamed in (MissingLevelPackage; cooked servers have every cell package and take any), and
  # the walk leaves the two leased cells far behind — so the harness client does not stream by its controller.
  WPCLIENT="-MusubiNitlNoWpSource"
fi
echo "== servers"
S2SPAWN=""; if [ "$CHAOS" = 1 ]; then
  S2SPAWN="-MusubiNitlSpawn=$((CELL + CELL / 2)),100${NITL_SPAWN_Z:+,$NITL_SPAWN_Z}"
  curl -fsS -X PUT "$API/v1/admin/dev/netsim" -H "x-musubi-server-key: $KEY" -H 'content-type: application/json' -d '{"latency_ms":60,"jitter_ms":20,"loss_pct":2}' > "$OUT/netsim.json" 2>&1 || curl -fsS -X POST "$API/v1/admin/dev/netsim" -H "x-musubi-server-key: $KEY" -H 'content-type: application/json' -d '{"latency_ms":60,"jitter_ms":20,"loss_pct":2}' > "$OUT/netsim.json" 2>&1
  echo "netsim: $(head -c 300 "$OUT/netsim.json")"
fi
SZ=""; [ -n "${NITL_SPAWN_Z:-}" ] && SZ=",$NITL_SPAWN_Z" # a map with a floor (the demo): spawn height, the Entry map has none
S1SPAWN="-MusubiNitlSpawn=$X0,100$SZ"; [ "$JOIN" = 1 ] && S1SPAWN="" # join: the node spawns the avatar from the policy
ue nitl-s1 -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="0,0" $S1SPAWN -MusubiNitlWalk="$WALK"
ue nitl-s2 -server ${NITL_SERVER_EXTRA:-} -MusubiNitl=server -MusubiCells="1,0" -MusubiNitlWalk="$WALK" $S2SPAWN
# chaos: the second client sits on s2 (its entity is parked in cell 1,0) and must be re-homed to s2b when s2 dies. It has
# to be connected *before* the kill, so in join mode it starts before the walking client (a cold editor takes 10-20 s to
# reach the relay; the kill point comes ~1 s after the walk starts)
ENTITY2=""
start_c2() {
  for i in $(seq 1 120); do ENTITY2=$((docker logs nitl-s2 2>&1 || true) | sed -n 's/.*nitl: spawned \([0-9a-f-]*\).*/\1/p' | head -1); [ -n "$ENTITY2" ] && break; sleep 1; done
  [ -n "$ENTITY2" ] || { echo "s2 never spawned the second entity"; exit 1; }; echo "entity2=$ENTITY2"
  echo "== client 2 (chaos)"
  ue nitl-c2 -game -MusubiNitl=client -MusubiEntity="$ENTITY2" -MusubiNitlWalk=0
  for i in $(seq 1 180); do (docker logs nitl-c2 2>&1 || true) | grep -q "relay .* open on $ENTITY2" && break; sleep 1; done
  (docker logs nitl-c2 2>&1 || true) | grep -q "relay .* open on $ENTITY2" || echo "== WARNING: c2 never opened its relay on $ENTITY2"
}
ENTITY=""
if [ "$JOIN" = 1 ]; then
  # s1 must hold its cell before the client joins, so the policy spawn lands on a live lease. `|| true` on every docker logs
  # pipe: grep -q / head close the pipe early, docker logs dies of SIGPIPE and under pipefail the && never fires — the wait
  # then runs its full 180 s and the quiet cells hibernate before the client arrives.
  for i in $(seq 1 180); do (docker logs nitl-s1 2>&1 || true) | grep -q "nitl: cell .* assigned=1" && break; sleep 1; done
  [ "$CHAOS" = 1 ] && start_c2
  echo "== client (join)"
  ue nitl-c -game -MusubiNitl=client -MusubiNitlJoin -MusubiNitlWalk="$WALK" ${WPCLIENT:-}
  for i in $(seq 1 180); do
    ENTITY=$((docker logs nitl-c 2>&1 || true) | sed -n 's/.*my avatar in .* is \([0-9a-f-]*\) .*/\1/p' | head -1); [ -n "$ENTITY" ] && break; sleep 1
  done
  [ -n "$ENTITY" ] || { echo "client never got its avatar (world_join)"; (docker logs nitl-c 2>&1 || true) | grep -i "musubi\|error" | tail -40; exit 1; }
fi
for i in $(seq 1 180); do
  [ -n "$ENTITY" ] && break
  ENTITY=$((docker logs nitl-s1 2>&1 || true) | sed -n 's/.*nitl: spawned \([0-9a-f-]*\).*/\1/p' | head -1); [ -n "$ENTITY" ] && break; sleep 1
done
curl -fsS "$API/v1/server/worlds/$WORLD" -H "x-musubi-server-key: $KEY" > "$OUT/world-after.json" 2>/dev/null; echo "world state: $(head -c 600 "$OUT/world-after.json")"
[ -n "$ENTITY" ] || { echo "s1 never spawned the entity"; (docker logs nitl-s1 2>&1 || true) | grep -i "musubi\|error\|warning: " | tail -40; exit 1; }
echo "entity=$ENTITY"
[ "$CHAOS" = 1 ] && [ -z "$ENTITY2" ] && start_c2
if [ "$JOIN" != 1 ]; then
  echo "== client"
  ue nitl-c -game -MusubiNitl=client -MusubiEntity="$ENTITY" -MusubiNitlWalk="$WALK" ${WPCLIENT:-}
fi
if [ "$CHAOS" = 1 ]; then
  # kill s2 when the walking pawn is 1-2 s from the border: its lease (2 s) is still valid, so the node starts the
  # handover towards a dead server; no ack must come back and the handover must be cancelled, not lost
  T0=$(date +%s); KILLED=""
  while [ $(( $(date +%s) - T0 )) -lt 100 ]; do
    # `|| true`: an empty 3 s window makes grep fail and pipefail+set -e would end the run right here
    PX=$( (docker logs --since 3s nitl-s1 2>&1 || true) | grep "nitl: server pawn .* id=$ENTITY " | tail -1 | sed -n 's/.* at \([-0-9]*\),.*/\1/p' || true)
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
if [ "$JOIN" = 1 ]; then chk nitl-c "my avatar in $WORLD is $ENTITY (spawned)"; chk nitl-c "relay .* open on $ENTITY" ; fi
chk nitl-c  "Welcomed by server"
# the relay PacketHandler: the engine's component chain is up on both ends (reserved bits / encryption component as configured)
chk nitl-c  "relay PacketHandler up (client)"
chk nitl-s1 "relay PacketHandler up (server)"
if [ "$ENCRYPT" = 1 ]; then
  chk nitl-c  "encryption component present"
  chk nitl-s1 "nitl: encryption token 'nitl'"
  chk $S2LOG  "nitl: encryption token 'nitl'"   # the new owner keyed the rehomed connection again
  chk nitl-c  "nitl: encryption ack"
  NE=$(grep -c "UNetConnection::EnableEncryption" "$OUT/nitl-c.log"); if [ "${NE:-0}" -ge 2 ]; then echo "PASS nitl-c: connection keyed by both owners ($NE)"; else echo "FAIL nitl-c: connection keyed by both owners ($NE)"; ok=0; fi
fi
chk nitl-c  "rehoming in place"
chk nitl-c  "rehomed in place"
LASTP=$(grep "client pawns=" "$OUT/nitl-c.log" | tail -1); case "$LASTP" in *"pawns=$PAWNS "*) echo "PASS nitl-c: $PAWNS pawn(s) at the end";; *) echo "FAIL nitl-c: $PAWNS pawn(s) at the end ($LASTP)"; ok=0;; esac
LASTX=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1 | sed -n 's/.* at \([-0-9]*\),.*/\1/p'); if [ -n "$LASTX" ] && [ "$LASTX" -gt 300 ]; then echo "PASS nitl-c: pawn kept moving after rehome (x=$LASTX)"; else echo "FAIL nitl-c: pawn kept moving after rehome (x=$LASTX)"; ok=0; fi
# the kept pawn is the same AActor*: the new owner's NetGUID table pre-registered it, its open bunch re-attached (no swap)
chk nitl-c  "kept actors pre-registered"
chk nitl-c  "rehome: reused"
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
# same instance: the pawn's name did not change and its client-local looping timer kept counting through the rehome
P1=$(echo "$BEFORE" | sed -n 's/.*client pawn \([^ ]*\) .*/\1/p'); P2=$(echo "$AFTER" | sed -n 's/.*client pawn \([^ ]*\) .*/\1/p')
if [ -n "$P1" ] && [ "$P1" = "$P2" ]; then echo "PASS nitl-c: same pawn actor before and after the rehome ($P1)"; else echo "FAIL nitl-c: same pawn actor before and after the rehome (before=$P1 after=$P2)"; ok=0; fi
L1=$(echo "$BEFORE" | sed -n 's/.* local=\(-*[0-9]*\).*/\1/p'); L2=$(echo "$AFTER" | sed -n 's/.* local=\(-*[0-9]*\).*/\1/p')
if [ "${L1:--1}" = "-1" ]; then echo "SKIP nitl-c: client-local timer (the project's pawn is not the NITL pawn)";
elif [ -n "$L2" ] && [ "$L2" -gt "$L1" ]; then echo "PASS nitl-c: client-local timer on the kept pawn ran through the rehome (local $L1 -> $L2)"; else echo "FAIL nitl-c: client-local timer on the kept pawn ran through the rehome (local $L1 -> $L2)"; ok=0; fi
# the player's own actors: the new owner announces PlayerController/PlayerState GUIDs after login (kind 2), so both stay the same instance
C1=$(echo "$BEFORE" | sed -n 's/.* ctrl=\([^ ]*\).*/\1/p'); C2=$(echo "$AFTER" | sed -n 's/.* ctrl=\([^ ]*\).*/\1/p')
if [ -n "$C1" ] && [ "$C1" != "none" ] && [ "$C1" = "$C2" ]; then echo "PASS nitl-c: same PlayerController instance before and after the rehome ($C1)"; else echo "FAIL nitl-c: same PlayerController instance before and after the rehome (before=$C1 after=$C2)"; ok=0; fi
Q1=$(echo "$BEFORE" | sed -n 's/.* ps=\([^ ]*\).*/\1/p'); Q2=$(echo "$AFTER" | sed -n 's/.* ps=\([^ ]*\).*/\1/p')
if [ -n "$Q1" ] && [ "$Q1" != "-" ] && [ "$Q1" = "$Q2" ]; then echo "PASS nitl-c: same PlayerState instance before and after the rehome ($Q1)"; else echo "FAIL nitl-c: same PlayerState instance before and after the rehome (before=$Q1 after=$Q2)"; ok=0; fi
# registered subobject: the owner's NetGUID table pre-registers it too, so the client keeps its Loadout instance (same name)
S1=$(echo "$BEFORE" | sed -n 's/.* sub=\([^ ]*\).*/\1/p'); S2=$(echo "$AFTER" | sed -n 's/.* sub=\([^ ]*\).*/\1/p')
if [ -z "$S1" ] || [ "$S1" = "-" ]; then echo "SKIP nitl-c: same subobject instance (the project's pawn has no Loadout)";
elif [ "$S1" = "$S2" ]; then echo "PASS nitl-c: same registered-subobject instance before and after the rehome ($S1)"; else echo "FAIL nitl-c: same registered-subobject instance before and after the rehome (before=$S1 after=$S2)"; ok=0; fi
# nothing fell back to the swap path on any client (an entity bound after the connection opened gets a table delta)
SW=$(grep -h "rehome: [1-9][0-9]* actors swapped" "$OUT"/nitl-c*.log | head -1)
if [ -z "$SW" ]; then echo "PASS nitl-c: no actor swapped for the new owner's copy on any client"; else echo "FAIL nitl-c: no actor swapped for the new owner's copy on any client ($SW)"; ok=0; fi
# the stable handle: taken from the first pawn, still resolves to the live possessed pawn, no actor change (same instance)
H=$(grep "nitl: handle " "$OUT/nitl-c.log" | tail -1 | sed 's/.*nitl: handle //')
case "$H" in *"changes=0 live=1") echo "PASS nitl-c: entity handle resolves to the live pawn after the rehome, no actor change ($H)";; *) echo "FAIL nitl-c: entity handle resolves to the live pawn after the rehome, no actor change ($H)"; ok=0;; esac
# replicated components in the binary iris state: the ghost on s2 saw the inventory before owning, the new owner carried on from it
EXPC=$(grep -o "first binary iris state exported ([0-9]* component" "$OUT/nitl-s1.log" | head -1 | grep -o "[0-9]*")
if [ "${EXPC:-0}" -ge 1 ]; then echo "PASS nitl-s1: replicated components exported in the iris state ($EXPC)"; else echo "FAIL nitl-s1: replicated components exported in the iris state (${EXPC:-0})"; ok=0; fi
# ghost roles: on the receiving server the entity is a ghost first (ROLE_SimulatedProxy=1, HasAuthority()=0), then the owner (ROLE_Authority=3, 1); on s1 the owner has authority
GR=$(grep "nitl: server pawn .* id=$ENTITY owned=0 role=" "$OUT/$S2LOG.log" | grep -o "role=[0-9]* auth=[0-9]*" | tail -1)
OR=$(grep "nitl: server pawn .* id=$ENTITY owned=1 role=" "$OUT/$S2LOG.log" | grep -o "role=[0-9]* auth=[0-9]*" | tail -1)
S1R=$(grep "nitl: server pawn .* id=$ENTITY owned=1 role=" "$OUT/nitl-s1.log" | grep -o "role=[0-9]* auth=[0-9]*" | head -1)
GHOSTROLE=1
# chaos: the pawn kept walking while s2 was dead and s2b adopts it well past the ghost margin — neither s1 nor s2b has a
# ghost phase for it, the role checks on ghosts cannot apply
[ "$CHAOS" = 1 ] && { GHOSTROLE=0; echo "SKIP nitl-s1/$S2LOG: ghost role checks (chaos: the retried handover lands past the ghost margin, no ghost phase)"; }
if [ -n "$GR" ]; then
  if [ "$GR" = "role=1 auth=0" ]; then echo "PASS $S2LOG: ghost is ROLE_SimulatedProxy without authority ($GR)"; else echo "FAIL $S2LOG: ghost is ROLE_SimulatedProxy without authority ($GR)"; ok=0; fi
else echo "SKIP $S2LOG: ghost role check (no ghost phase)"; fi
if [ "$OR" = "role=3 auth=1" ]; then echo "PASS $S2LOG: owner has ROLE_Authority after the handover ($OR)"; else echo "FAIL $S2LOG: owner has ROLE_Authority after the handover (${OR:-none})"; ok=0; fi
if [ "$S1R" = "role=3 auth=1" ]; then echo "PASS nitl-s1: owner has ROLE_Authority before the handover ($S1R)"; else echo "FAIL nitl-s1: owner has ROLE_Authority before the handover (${S1R:-none})"; ok=0; fi
S1G=$(grep "nitl: server pawn .* id=$ENTITY owned=0 role=" "$OUT/nitl-s1.log" | grep -o "role=[0-9]* auth=[0-9]*" | tail -1)
if [ "$GHOSTROLE" = 1 ]; then
  if [ "$S1G" = "role=1 auth=0" ]; then echo "PASS nitl-s1: old owner became a ROLE_SimulatedProxy ghost after the handover ($S1G)"; else echo "FAIL nitl-s1: old owner became a ROLE_SimulatedProxy ghost after the handover (${S1G:-none})"; ok=0; fi
  case "$VER" in 5.8*) ;; *) chk "$S2LOG" "ghost PreReplication as SimulatedProxy";; esac
fi
if [ "$WP" = 1 ] && [ "$CHAOS" != 1 ]; then
  # servers: WP streaming on, and the streamed-in markers follow the lease — s2 (cell 1,0 + margin) has 1,0 and not 11,0,
  # s1 (0,0) has 0,0 and not 11,0; nothing is loaded for cells nobody leases. The engine adds its own server extra radius
  # (wp.Runtime.ServerStreamingSourceMinimumExtraRadius / 2 × source location quantization ≈ 8 m) on top of the margin,
  # which on this toy 2.56 m grid is a few cells — hence a far marker 11 cells out.
  chk nitl-s1 "IsServerStreamingEnabled = 1"
  chk nitl-s1 "world partition: simulator registered as the streaming source"
  wpm() { grep "nitl: wp partitioned=1 leased=$2 " "$OUT/$1.log" | grep -o "markers=.*" | tail -1; }
  WM2=$(wpm "$S2LOG" "1,0"); WM1=$(wpm nitl-s1 "0,0")
  case "$WM2" in *"1,0"*) echo "PASS $S2LOG: wp streamed in the leased cell 1,0 ($WM2)";; *) echo "FAIL $S2LOG: wp streamed in the leased cell 1,0 (${WM2:-none})"; ok=0;; esac
  case "$WM2" in *"11,0"*) echo "FAIL $S2LOG: wp did not stream a far cell 11,0 ($WM2)"; ok=0;; *) echo "PASS $S2LOG: wp did not stream a far cell 11,0";; esac
  case "$WM1" in *"0,0"*) echo "PASS nitl-s1: wp streamed in the leased cell 0,0 ($WM1)";; *) echo "FAIL nitl-s1: wp streamed in the leased cell 0,0 (${WM1:-none})"; ok=0;; esac
  case "$WM1" in *"11,0"*) echo "FAIL nitl-s1: wp did not stream a far cell 11,0 ($WM1)"; ok=0;; *) echo "PASS nitl-s1: wp did not stream a far cell 11,0";; esac
  chk nitl-c "nitl: wp partitioned=1"
fi
if [ "$JOIN" = 1 ]; then echo "SKIP nitl-s: inventory / iris-seed / subobject-seed checks (the node-spawned avatar carries no NITL seed; the client did see the new owner's FastArray and subobject above)"; else
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
# registered replicated subobject (AddReplicatedSubObject + RegisterMusubiSubObject, the NITL pawn's "Loadout"): its state
# must arrive on the new owner with the handover and reach the client from there; a game pawn without one is SKIP
M1=$(grep -n "nitl: server pawn .* id=$ENTITY owned=1 " "$OUT/nitl-s1.log" | awk -F: -v n="$HO" '$1<n' | grep -o "ammo=-*[0-9]*" | tail -1 | cut -d= -f2)
M2=$(grep "nitl: server pawn .* id=$ENTITY owned=1 " "$OUT/$S2LOG.log" | grep -o "ammo=-*[0-9]*" | head -1 | cut -d= -f2)
MC=$(grep "nitl: client pawn .* id=$ENTITY " "$OUT/nitl-c.log" | tail -1 | grep -o "ammo=-*[0-9]*" | cut -d= -f2)
if [ "${M1:--1}" = "-1" ]; then echo "SKIP nitl-s: registered subobject carried across the handover (pawn has no Loadout subobject)"
elif [ -n "$M2" ] && [ "$M1" -ge 1 ] && [ "$M2" -ge "$M1" ]; then echo "PASS nitl-s: registered subobject carried across the handover without reset (ammo s1 $M1 -> s2 $M2)"; else echo "FAIL nitl-s: registered subobject carried across the handover without reset (ammo s1 ${M1:-?} -> s2 ${M2:-?})"; ok=0; fi
if [ "${M1:--1}" = "-1" ]; then :; elif [ -n "$MC" ] && [ "$MC" -ge "$M1" ]; then echo "PASS nitl-c: client sees the registered subobject from the new owner (ammo=$MC)"; else echo "FAIL nitl-c: client sees the registered subobject from the new owner (ammo=${MC:-?}, s1 had $M1)"; ok=0; fi
fi
if grep -q "already has a push id" "$OUT/nitl-s1.log" "$OUT/$S2LOG.log"; then echo "FAIL nitl-s: no push id conflict between the Iris export and the GameNetDriver"; ok=0; else echo "PASS nitl-s: no push id conflict between the Iris export and the GameNetDriver"; fi
if [ "$CHAOS" = 1 ]; then
  # the kill lands 1-2 s before the crossing and s2's lease is 2 s: usually the node still starts the handover towards the
  # dead s2 and must cancel it (no ack); when the lease ran out first the handover goes straight to s2b — not a failure
  if grep -q "nitl: handover $ENTITY cancelled" "$OUT/nitl-s1.log"; then echo "PASS nitl-s1: handover towards the dead s2 cancelled, not lost"
  else echo "NOTE nitl-s1: no cancelled phase (s2's lease expired before the crossing; the handover went straight to s2b)"; fi
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
