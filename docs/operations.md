# Operating Musubi

CLI, configuration, measured numbers, micro-benchmarks and the chaos stand. Moved here from the README; every number is from a run, not a promise.

## CLI and configuration

| command | what it does |
|---|---|
| `musubi doctor` | config sanity (built-in secrets are refused outside `musubi dev`), database + pending migrations, data dir, ports; exit 1 if the server would not start |
| `musubi migrate` | apply migrations and exit |
| `musubi export --out dump.jsonl` / `musubi import --input dump.jsonl [--replace]` | move game data between environments (every `musubi` table except runtime state, foreign-key order, idempotent import) |
| `musubi migrate-playfab --from ./export [--replace]` | import PlayFab (`GetPlayerCombinedInfo` JSON/JSONL, `{"Players": [...]}` wrappers) or Unity Gaming Services (Authentication + Cloud Save + Economy + Leaderboards stitched per player) exports: accounts with stable ids, linked platform identities, user data → storage (`playfab_user_data` / `playfab_readonly` / `cloud_save`), currencies → wallets, inventory, statistics → leaderboards; re-runs are idempotent and the old id keeps logging in via `POST /v1/server/auth/custom {"id": "playfab:<PlayFabId>"}` |
| `musubi bench --url … --clients 50 --duration-secs 10 --scenario http\|storage\|ws\|chat\|match\|world\|all [--match-size 8]` | load generator with p50/p95/p99, throughput, errors and 429s (`chat`: rooms of 50, send → own echo; `match`: relayed matches of `match_size` players at 30 Hz, relay latency measured at the receivers) |
| `musubi module publish NAME game.wasm` / `module canary NAME game.wasm --percent 10 [--accounts a,b]` / `module promote NAME [--drop]` / `module rollback NAME 3` / `module versions [NAME]` | ship WASM modules to a running cluster: straight to active, or as a canary taking a share of RPCs (plus pinned accounts) that you promote or drop; roll back to any earlier version |
| `musubi fleet push fleet.toml` / `musubi fleet list` | create the fleet in the file or update the existing one of that name (buffer/min/max/policy); a `[build]` section adds a build version — canary (`canary_percent > 0`) or active |
| `musubi deploy [--file musubi.deploy.toml]` | one manifest with `[[modules]]` (`name`, `path`, `canary_percent`) and `[[fleets]]` (same shape as `fleet push`), applied in order — the CI step after `cargo build --target wasm32-wasip1` |
| `musubi config --format json` | effective configuration (secrets included — do not paste it anywhere) |

Server-to-server keys: `http.server_key` is the bootstrap admin key; issue scoped keys with
`POST /v1/admin/keys {"name","role":"server"|"admin"}` and revoke them with `DELETE /v1/admin/keys/{id}`.
Clean-up of events/deliveries/audit/notifications/sessions runs on the `retention` lease
(`[retention]`, days per table; chat history per channel kind — `chat_room_days` 90, `chat_group_days` / `chat_direct_days` 0 = forever, `chat_days` > 0 overrides all three; room messages are written to history only when the sender joined with `persistence: true`). Nodes drain on SIGTERM or `POST /v1/admin/cluster/nodes/{id}/drain`.

### Measured (not promised)

`musubi bench`, **release** build, one node on an 8-vCPU / 31 GB VM with Postgres 16 in Docker on
the same box, 200 clients for 30 s, per-account `[limits]` off (otherwise the limiter is what you
measure):

| scenario | ops | ops/s | p50 | p95 | p99 | max | errors |
|---|---|---|---|---|---|---|---|
| HTTP `GET /v1/account` | 162 370 | 5 412 | 7.1 ms | 11.4 ms | 14.8 ms | 42.6 ms | 0 |
| storage write + read-back | 129 444 | 4 315 | 8.8 ms | 15.1 ms | 19.8 ms | 56.8 ms | 0 |
| WebSocket ping | 1 056 846 | 35 228 | 0.97 ms | 2.4 ms | 3.8 ms | 29.3 ms | 0 |
| chat message → delivered (rooms of 50) | 559 927 | 18 664 | 0.86 ms | 2.4 ms | 4.2 ms | 26.4 ms | 23* |
| relayed match input → echo | 253 936 | 8 465 | 1.35 ms | 3.4 ms | 5.1 ms | 33.1 ms | 0 |

\* the 23 chat "errors" are sessions the node closed as slow consumers (see below): one bench
process pretending to be 200 clients cannot drain ~1 M chat deliveries per second.

World meshing, same box, **release** build, 30 s runs, 4 simulators per world over the stream
(`--sims 4`, entities drifting across cell borders), players at 200 units radius, 30 Hz. "before"
is the tree as of `bebbbcf`, "after" has the three fixes described below the table:

| run | clients | worlds × entities | wire | frames/client/s | client delta p50 / p95 / p99 | node fan-out p50 / p99 | RL tick mean / p99 | handovers done / asked | node CPU |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 200 | 2 × 500 | json | 29.8 | 34 / 43 / 61 ms | 2.5 / 25 ms | 0.23 / 2.5 ms | 359 / 359 | — |
| 2 before | 1 000 | 4 × 2 000 | json | 26.5 | 50 / 75 / 123 ms | 10 / 100 ms | 1.0 / 25 ms | 5 103 / 5 188 | 154 % |
| 2 after | 1 000 | 4 × 2 000 | json | **29.0** | **33 / 53 / 71 ms** | **5 / 50 ms** | 0.48 / 10 ms | 4 818 / 4 818 | 128 % |
| 2b | 1 000 | 4 × 2 000 | binary | 28.6 | 34 / 60 / 81 ms | 10 / 50 ms | 0.6 / 10 ms | 4 959 / 4 959 | 154 % |
| 3 before | 2 000 | 4 × 4 000, 8 sims | json | 12.3 | 65 / 240 / 352 ms | 25 / 250 ms | 2.2 / 25 ms | 6 982 / 13 744 | 164 % |
| 3 after | 2 000 | 4 × 4 000, 8 sims | json | **23.6** | **39 / 94 / 134 ms** | **25 / 100 ms** | 1.5 / 25 ms | **8 568 / 8 574** | 225 % |
| 3, 2 nodes, players on home, Postgres bus | 2 000 | 4 × 4 000, 8 sims | json | 24.0 | 35 / 87 / 196 ms | 25 / 100 ms | 2.3 / 25 ms | 2 099 / 6 055 | home 47 %, peer 22 % |
| 3, 2 nodes, players split, Postgres bus | 2 000 | 4 × 4 000, 8 sims | json | 13.8 | 35 / 198 / 706 ms | 10 / 50 ms | 2.0 / 10 ms | 2 096 / 5 554 | home 65 %, peer 33 % |
| 3, 2 nodes, players on home, QUIC bus | 2 000 | 4 × 4 000, 8 sims | json | 28.3 | 31 / 72 / 192 ms (max 0.44 s) | 10 / 50 ms | 1.6 / 10 ms | 3 269 / 7 249 | home 110 %, peer 26 % |
| 3, 2 nodes, players split, QUIC bus | 2 000 | 4 × 4 000, 8 sims | json | 25.5 | 33 / 93 / 187 ms (max 0.75 s) | 10 / 50 ms | 1.5 / 10 ms | 3 524 / 6 476 | home 66 %, peer 12 % |

The two-node rows are the same stand with a second `sim`-role node on the same VM and
`--controller '{"offload_above_cost": 800}'`: the home moves ~40 cells (of 4 × 25) to the peer,
the bench simulators follow the lease redirects and the peer ticks and fans out its cells
(70–160 k frames in 30 s). The first two rows are kept as a warning: node 1 of that stand had no
`node.bus_listen`, so every one of its inter-node messages went over the Postgres `NOTIFY`
fallback (one round trip to the database each, 7 900 bytes max) — that, not the design, is the
13.8 fps and the 22 s backlog with players split. Three things changed for the QUIC rows:
messages to sessions on other nodes (`Wire::Session`, i.e. world deltas to remote players) get
their own bus lane and are batched per drain like replica deltas (they used to go one message
per session per tick on the control lane, in front of the handovers; batching is off on the
Postgres fallback, where a batch would exceed the cap and be dropped whole); a node the
directory names as a cell's host now takes the cell on the first request for it (`ensure_hosting`,
with the cell resolved from `musubi.worlds` when the shard is not up yet, and a follower whose
directory replica lags asks the leader) instead of redirecting back to the home — a simulator
redirected during an offload used to bounce between the two nodes until "too many redirects";
and the bench stand survives cells moving while it is still spawning (per-cell leases, a 409 on
spawn re-leases on the new host). Result with players split over both nodes: 23 fps per client,
same p99 as one node, the peer now does more of the work than the home (32 % vs 48 % of a core)
and the home's send path is no longer the ceiling (fps per client moves between 17 and 25
across runs of the same build; the rows are single runs, the split row shows the range seen).
Still honest: this is two nodes on one VM, and cross-node handovers remain the open problem —
~3 000–3 400 complete per run against ~10 000–13 000 asked (one node: all of them; the counter
now includes completions acked from another node, it used to count only the simulator's ack
path). The funnel is instrumented (`musubi_world_handover_recv_total{path}` on the receiver,
`musubi_world_handover_acks_total{path}` on the sender, `musubi_fanout_published_total{lane,kind}`
on the bus) and it moved the picture from guesses to numbers: the control lane of the bus used to
carry ~148 000 `world_subscribe` re-sends a run (clients move their interest as they walk, each
one a message to the shards of a split world) ahead of the ~12 000 handovers, so a handover
reached the peer up to a minute late and the sender re-shipped it (55 000 duplicate deliveries a
run) — those re-sends now go on the batched input lane, the control lane carries handovers only,
duplicates are 16 a run; a handover ships the full entity (a delta against a ghost the receiver
no longer held was refused one time in five); a world deleted on its home is dropped on every
node (the peer kept the shards of 198 deleted bench and test worlds); the remote-handover
timeout is 30 ticks (`remote_handover_timeout_ticks`), acks travel on the parallel lane, the
control lane keeps 8 sends in flight with order restored on the receiver — and a gap is no longer just waited out: a receiver missing frames for 50 ms asks the sender (`OrderNack`, once per 200 ms per gap), the sender replays them from the last 256 control frames it kept, replays other receivers already have are dropped as duplicates, and only a gap still open after 500 ms is skipped (a frame of it arriving later is still dispatched, once) — `musubi_bus_order_{nacks,replayed,dups,gaps}_total`. What is left is measured too, down
to single entities (`hotrace` debug lines on both nodes, one entity in sixteen): a border
handover shipped to the peer is acked while still in transit 264 times of 266 — the cross-node
path itself works and takes ~60 ms; the ~6 400 "late" acks a run are the peer's acks to the
forced migrations of offloaded cells (the home drops those entities as it ships them, so an ack
finds nothing — benign); and the largest loop was a refusal: the peer turned an entity away when
the hosted target cell had no simulator lease yet (the bench simulator was still following the
offload redirect), the sender re-offered it every tick (~60 ms) for as long as that took. A
hosted cell without a lease now takes the entity and holds it, as an unleased cell on one node
does (`unowned_cell` in the receiver's funnel: 11 500 of 16 600 arrivals in two runs) —
completions went from ~3 000 to 3 500–4 100 of ~5 800 asked. A receiver whose cached cell directory says the cell is not
its own now asks the leader before turning the entity away (`here_after_fresh_lookup`: 353 of
521 such arrivals in a run were in fact its cells; the 168 `not_here_confirmed` are refused at
once instead of silently, so the sender re-targets on the next tick rather than after the
timeout) — 3 750 of 5 801 asked completed in that run. The bench's "asked" column used to count every
`handover_request` event, and an entity the home re-offers on the next tick while its first
request is open appeared again; it now counts distinct requests and reports the repeats as
`handover_asks_repeated` (forced migrations of offloaded cells never were in it — they reach
the simulator as `cell_released`). Counting how a request ends
(`musubi_world_handover_ended_total{path}`) showed the real inflation: ~15 000 local requests
per run expired after `handover_timeout_ticks` because the simulator's ack took longer than
10 ticks on this shared VM, and the entity was asked again next tick while the late ack was
thrown away. A late ack is now accepted when the entity still stands in a cell the acking
simulator owns — distinct requests per run 10 771 → 4 202 with 3 245 completed (77 %, was
32 %), local expiries 15 257 → 7 465, and each expiry is closed by the ack instead of a
re-ask. The simulator stream itself no longer makes an ack wait: frames used to be written
in the same loop that read the simulator's commands, so an ack sat behind the full-state frame
being sent — frames now leave through a bounded channel on their own task, a tick that finds it
full is skipped (`musubi_world_sim_frames_skipped_total`; events are drained only when a frame
is built, so none are lost) and the ack's age is measured
(`musubi_world_handover_ack_latency_ticks`). Honest result: on this stand the server's writer
was almost never the blocker (34 skipped frames a run) — the simulators here are 32 bots in one
bench process parsing full-state JSON at 70 % CPU, so the split alone moved expiries only
15 406 → 13 530; the timeout default did the rest (next bullet list). The frame itself was the
simulators' real cost: every tick carried every entity of every leased cell. `stream?delta=1`
sends per cell only what changed since the simulator's previous frame (`owned`/`ghosts` with the
changed entities, `gone` with the ids that left, `full: true` on a cell's first frame) and the
simulator keeps the view; same stand: 14.6 M entity records a run became 0.49 M (30× less), the
bench process 71 → 58 % CPU, node 28 → 22 %, expiries 8 204 → 7 276, 0 errors — and the bots
still read ~19 of 30 frames a second, so their loop, not the frame, is the next thing on the
stand side. The control lane's gap replay (`OrderNack`, above) was then put on the same stand: a
players-split run showed no gaps and no nacks on either node and two duplicate frames dropped on
the peer — the QUIC bus loses nothing on one VM, so the replay is exercised by its unit tests and
is there for the Postgres fallback, reconnects and lossy links (26.7 fps per client, p99 171 ms,
3 630 of 5 723 asked handovers completed, 0 errors on that run). Backpressure for the
remote players' deltas sits at the producer, not in the queue: a frame to a session on another
node cannot be dropped once built (the world has already marked the client as knowing it —
spawns and positions would never be resent), so instead the home skips building a new delta for a
session whose previous frame has waited on the session lane longer than `SESSION_CONGESTED_AFTER`
and the client's interest keeps the change for the next one (`musubi_world_deltas_deferred_total`,
`musubi_fanout_queue_depth{lane}`). Measured honestly: gating at 100 ms bounded the lane (worst
wait 11.5 s → 4.8 s) but deferred 465 000 deltas a run and cost every remote player a fifth of
their frame rate (26.7 → 21.3 fps) — the lane's send throughput is the ceiling, not the queue —
so the gate is one second: the same run then holds 26.6 fps per client (p99 189 ms, 0 errors),
defers 424 deltas — only the ones that would have arrived useless — and the lane's worst wait is
9.0 s instead of 11.5 s. Which says plainly that the remaining backlog is the session lane's own
throughput (one send in flight, a thousand frames serialized per tick on the home), the next
thing to take apart. First probe, measured and kept out of the code: four session lanes sharded
by session id (order only matters per session) changed nothing — 25.7 fps, worst wait 12.3 s,
average 7.3 s, i.e. the lanes queue behind each other on the one QUIC connection. The numbers
that do point somewhere: a 64-frame session batch takes 23 ms on average (136 ms worst) to
serialize and push, the home sends ~59 MB/s to the peer over the run, and the peer applies what
arrives with an 8.6 ms average lag — the cost is on the sender's batch path (JSON of a thousand
deltas a tick plus the send into the peer's flow-control window), not at the receiver. Split further (`musubi_fanout_batch_ms{lane,phase}`): serializing a
session batch costs 0.26 ms, sending it 3.1 ms — twelve times more, and the bulk lane's sends
added up to 48 s of a 40 s run (1.1 ms × 42 000 batches), so both lanes are simply saturated by
the send call, which returns only when the peer has read the frame. So the bus now keeps one long-lived QUIC stream per
peer and priority lane and writes length-prefixed frames on it — a send returns when the bytes are
in the stream (flow control is the only wait) instead of after the peer has read them (`stopped()`
per frame was the round trip), order per lane is the stream's, legacy one-frame-per-stream peers
are still read. Measured on the same stand: 28.0 fps per client (p95 84 ms, p99 166 ms, 0 errors),
the bulk lane's backlog is gone (38 s worst wait → no slow sends at all), the session lane's is
not (worst wait 6.6 s, a 64-frame batch still takes 2.5 ms to write) — and the peer's inbound lag
rose from 8.6 to 23.5 ms: the wait moved to the receiver, which reads and parses each lane stream
sequentially. Parsing was then taken off the stream reader (the reader only moves bytes and
releases the window, a worker per stream parses and dispatches in order, `musubi_bus_ingest_ms`):
the peer's inbound lag fell to 15.9 ms, fps stayed at 28.0 — and the measurement says the parse
was never the cost (15 µs a frame). A 64-frame write still takes 2.7 ms, which with 15 µs of work
behind it is flow control: the next suspect is the QUIC connection's receive and send windows for
~60 MB/s on one connection. Raised (16 MB per stream, 64 MB per connection, both ends, `bus_transport`): 28.4
fps per client, p99 150 ms (the best of this series), 0 errors, the session lane's worst wait
4.1 s (was 6.1 s) — but a write still costs 2.4 ms, and the peer's inbound bulk queue now lags
224 ms on average (was 16 ms): the window let the home push replica and ghost deltas faster than
the peer's single apply worker takes them under the world locks. The backlog has moved for the
last time to where the work actually is — applying bulk deltas on the receiver — and that worker
was then split per world (one apply worker per world, order within a world kept): the peer's
inbound bulk lag halved, 224 → 109 ms average, with fps and p99 inside the run-to-run spread
(26.9 fps, p99 191 ms, 0 errors; 28.4 / 150 the run before — the stand's variance between runs is
about that wide, so only the lag figure is a result). What remains on the receiver is the per-world
lock the delta is applied under, and that is a world-tick question, not a transport one. The bus frame
itself was the fps ceiling: every message was a `serde_json::Value` — serialized twice on send,
decoded twice on receive — and the home's session and bulk lanes ran 15–25 s behind under this
load. The payload is now carried as the sender's bytes (`RawValue`, one serialization per hop):
the bulk lane no longer falls behind, the session lane's worst wait went from 26 s to 10 s,
inbound dispatch lag averages 3 ms, and the two rows above moved from 17–25 fps with
multi-second stalls to 25.5 / 28.3 fps with a worst frame under a second — on two nodes sharing
one VM with the bench process, so the absolute numbers say less than the trend.

Read it as: the simulation step (`musubi_world_tick_duration_seconds`) is never the problem —
1.5 ms mean with 4 000 entities and 8 simulators handing entities to each other 280 times a
second. What fell over first was the **fan-out**, and `MUSUBI_TICK_TRACE=1` showed where: of a
40–110 ms fan-out per tick, almost all of it was `World::snapshot()` — the per-tick copy of the
world that the delta build reads — deep-cloning 4 000 entities' component maps (JSON values) on
the world's single thread, with the other cores idle (run 3 before: 1.6 of 8 cores). Three fixes:

- entity components live behind an `Arc` with copy-on-write (`Arc::make_mut` on the entities
  that change in a tick), so the snapshot is 4 000 reference bumps instead of a deep copy;
- the per-client delta scan runs in parallel chunks on the rayon pool once a world has more
  than `PAR_DELTAS_MIN` (128) subscribers (small worlds stay sequential: the scheduling cost
  exceeded the gain there);
- `handover_timeout_ticks` default 3 → 10: a simulator on the stream answers on its next frame,
  and 100 ms cancelled half of the handovers under load (run 3 before: 6 982 of 13 744 asked —
  the rest were rolled back and re-requested); at 10 ticks every handover completes.
- `handover_timeout_ticks` default 10 → 30 (same as the remote timeout): measured on the
  two-node stand (`musubi_world_handover_ack_latency_ticks`), the acks that made it in time
  peaked at 9–10 ticks and ~13 500 a run came later; at 30 the in-time acks sit at p50 5 /
  p99 19 ticks and expiries halve (13 530 → 8 204) — the rest are simulators that take over a
  second to answer on a 70 %-CPU bench process, and a late ack still completes them.

Also fixed on the way: a session whose outbound queue was full used to spill its message onto
the cluster bus (whose Postgres `NOTIFY` lane caps payloads at 7 900 bytes — the log filled with
`bus message realtime exceeds 7900 bytes` under load); it is now dropped locally and counted in
`musubi_session_dropped_total{reason="queue_full"}`. The binary wire cuts bytes 3.6× (59 → 16.5
MB/s at run 2) and is the right default for games; it does not change the fan-out ceiling. What
remains at 2 000 players is the one-home-node replication layer per world — the next item.
The 0.1–0.5 % `sim_moves_err` in the stream runs are `apply`s a simulator sent one tick after it
lost the entity to a neighbour (`not authoritative`) — expected and harmless; the new owner
carries on from the acked state.

Two things made the realtime rows what they are. The request state used to be a flat struct of
~50 reference-counted fields cloned by every middleware layer; behind one `Arc`,
`GET /v1/account` alone with all 200 clients went from 5 863 to 24 931 ops/s (p50 32 → 7.6 ms)
and the node from four cores to ~1.2, with Postgres (~1.7 cores) now the ceiling. And accepted
sockets run with `TCP_NODELAY`: a room fan-out is many small frames per connection, so Nagle plus
the peer's 40 ms delayed ACK had pinned chat at p50 44 ms and 6k messages/s whatever the CPU did;
without it the same run does 43 598 messages/s at p50 3.0 ms. Set it in your own clients too.

A session whose outgoing queue (256 messages) overflows is closed with
`unavailable: slow consumer: outgoing queue full` and counted in
`musubi_session_send_full_total`, like Nakama does — a gapped stream is worse than a reconnect,
and the client resumes with its subscriptions.

`database.max_connections` (default 32, sized so three local nodes fit under Postgres' default
100) is the next knob: at 96 the same box does 28 439 ops/s on `GET /v1/account` and 17 108 ops/s
on storage write+read-back (200 clients, one scenario at a time) against 24 931 / 14 674 at 32.

Where the rest of the time goes (same box, 48 connections, storage write+read-back at ~15 000
req/s): the node burns 1.1–1.5 cores, Postgres 2.2, and 2–3 cores stay idle; sampled thread states
show 1–2 of the node's 10 threads runnable and `pg_stat_activity` shows most backends in
`ClientRead` — the three processes are passing small messages to each other and the latency is
the box's scheduling and loopback stack, not any one of them. Per node core that is ~21 000
`GET /v1/account` and ~12 000 storage requests a second, against the 10 000 / 5 000 per vCPU the
design aimed for. Two things that do move it: pointing `database.url` at the container's IP
instead of the published port (Docker's userland proxy copies every byte) is +13 % and
‑1.4 ms p50; and the storage layer now spends one round trip where it used to spend several — a
batch read is a single `unnest` join in request order, and a write is one guarded
`insert … on conflict do update … returning version` (permission, CAS and create-only checks are
in the `where`), with the current row fetched only when the guard rejected. On loopback that is
noise; with Postgres a network hop away each round trip is the whole budget.

`musubi soak` on the same VM, release build, 3 nodes, 24 clients, 6 worlds split across two nodes
each with 20 entities shuttling over the border, 75 s, SIGKILL of a node every 12 s (3 incidents):
survivors marked the victim dead after 4.7 s, its worlds and matches were re-homed 4.7 s (p50) to
10.8 s (max) after the kill, every entity of every split world was accounted for on a survivor
again 4.7 s (p50) / 10.8 s (max) after the kill with 0 lost and 0 duplicated, the restarted
process rejoined in 1.0–2.0 s (one slow rejoin at 8.5 s); 25 175 verified storage writes with
0 lost, 8 WS sessions resumed (6 fresh reconnects), 5 537 match inputs echoed with 0 resets. An
earlier run of this stand found duplicates after a re-home (two border worlds with 36 and 33
entities instead of 20): a cross-node handover cancelled during the failover kept its copy while
the new host had already accepted it, and control messages were queuing tens of seconds behind
chat and replication traffic in a single bus queue. Adoption announcements answered with drops,
directory-based ownership checks and separate bus lanes are what made the numbers above settle —
the stand exists precisely so such numbers are not promised away. Run both against your own
deployment; numbers from real hardware and a real network are the ones that matter.

### Micro-benchmarks (criterion)

Hot paths have criterion benches that run on every push and pull request (`bench` job in
`.github/workflows/ci.yml`): `musubi-mesh` (handover detection, per-tick advance, cell view, client
delta at 1k / 10k entities), `musubi-proto` (envelope encode/decode in JSON and postcard) and
`musubi-auth` (argon2 hash/verify, password policy). The job benches the base commit (PR base, or the previous push) on the same runner first, then HEAD against it — comparing across GitHub's shared runners is meaningless, their CPU models differ by 2× on the same code — and `scripts/bench-compare.py 5` fails when a bench's mean is more than 5 % slower after dividing out the drift of the code-independent `calibrate` bench, even at the optimistic end of the 95 % interval, and by at least 100 ns (sub-microsecond benches still shift by a few tens of ns within one machine; `bench-compare.py 5 0` disables the floor). Suspects are re-run once before failing. Locally: `cargo bench -p musubi-mesh -p musubi-proto -p musubi-auth --benches -- --save-baseline main`
on the old code, `cargo bench … -- --baseline-lenient main` on the new code, then `python3 scripts/bench-compare.py`.

### Chaos stand (`musubi soak`)

Failover claims are measured the same way latency is. `musubi soak` spawns a real local cluster of
`musubi-server` processes on one Postgres, homes persistent worlds across them, puts resumable WS
sessions and write-then-read-back storage clients on it, then SIGKILLs or drains a random node every
few seconds and restarts it:

```sh
musubi soak --database-url postgres://musubi:musubi@localhost:5433/musubi_soak \
  --nodes 3 --clients 12 --worlds 6 --duration-secs 75 --kill-every-secs 12 --mode mixed
```

It reports, per incident, how long the survivors took to drop the node, how long until every world it
owned was homed elsewhere and how long the restarted process took to rejoin, plus client errors,
reconnects and lost writes (a write acknowledged by one node but not readable from another). The
command exits non-zero on any lost write or incident that did not settle within 60 s. Set
`MUSUBI_SOAK_NODE_LOG_DIR` (and `MUSUBI_SOAK_NODE_LOG_LEVEL`) to keep the nodes' logs.

Measured on the same debug build / VM as the numbers above, two 75 s runs, 3 nodes, 12 clients, 6
worlds, 7 incidents in total:

| incident | survivors drop the node | worlds homed elsewhere | restarted node back |
|---|---|---|---|
| SIGKILL (follower or leader) | 4.8 – 8.7 s | 5.7 – 9.3 s | 1.1 – 2.0 s |
| drain via admin API | 0.1 s | 3 – 5 s | 0.2 – 1.2 s |

Storage: 33 287 verified writes, 0 errors, 0 lost writes. WS: 17 445 pings answered, 17 sessions
cut by a kill or drain. Since resumable sessions keep a record in Postgres, a cut session is rebuilt
on whichever node the client reaches next (same session id and token, joins and follows replayed):
in a later 60 s mixed run all 6 cut sessions resumed, none reconnected fresh — the client has to
retry for a few seconds after a kill, because the cluster hands a session over only once SWIM has
declared its node dead (`MUSUBI_SOAK_DEBUG=1` prints each rejected attempt). The SIGKILL numbers are SWIM's suspect timeout plus one
failover round, which now starts the moment the death verdict lands (it used to wait for the next
15 s tick, making it 11 – 12 s); a drained node announces its departure, so its worlds move an order
of magnitude faster. Open item, now with a trace: in two of roughly twenty kills on this VM the re-home did not settle within the 60 s budget. Reproduced in 1 of 8 kill-only runs with the survivors' Raft/world/member state dumped: the Raft group had been churning elections the whole run (terms 6 → 141 with no new log entry, 142 elections against 3–8 in the clean runs) because vote RPCs to a *live* leader timed out after 1.5 s — that run shared the 8 cores with a `cargo clippy` build, i.e. CPU starvation, not a network fault (one stale-certificate handshake failure after a restart, otherwise none). When the leader was then killed, the two survivors never formed a quorum within 60 s and every world the leader homed stayed unreachable (`leader: null` on both). The failover round needs directory writes, so a leaderless group blocks re-homing entirely. Next: Raft timing that survives a starved box (heartbeat/election as cluster config, vote RPC timeout bounded by the election timeout) and a controlled starvation experiment on the stand. Done, with a twist: Raft heartbeat/election are now cluster config (`node.raft_heartbeat_ms`, `node.raft_election_min_ms`, `node.raft_election_max_ms`, defaults 500/1500/2500) and three kill-only runs under eight busy-loop processes on the 8-core VM gave: defaults starved — 4 elections, 0 lost; defaults unstarved — 9 elections, 0 lost; 1000/3000/5000 starved — 5 elections but one re-home never settled. That failure is not Raft: SWIM on the surviving follower flapped the *new leader* Alive → Suspect → Dead under starvation (probe 500 ms, suspect 3 s), its failover rounds then listed the live leader as dead, and the bus refuses to dial a SWIM-dead peer, so every directory write it needed was turned away while the leader itself timed out confirming leadership against the killed node still in its replication set. Busy loops do not reproduce the election churn of the clippy run (that build starves memory and IO, not just cores). Next: SWIM timings that tolerate a starved box and a bus that still dials the Raft leader regardless of SWIM state. Done for the verdict itself: a SWIM suspect whose Postgres heartbeat is still fresh (`DEAD_AFTER` 15 s) is kept suspect for one more window instead of declared dead (`musubi_swim_death_vetoed_total`; a bus partition with a live heartbeat is still declared dead after the second window, ~6 s) — two independent signals have to agree before a failover starts against a node. Under the same eight busy loops both configs then settle every kill (1000/3000/5000: 3 kills, 0 lost, 15 vetoes; defaults: 3 kills, 0 lost, 14 vetoes). The price is honest: a real death is now confirmed by the stale heartbeat, so detection under starvation went from ~5–6 s to 10–12 s; on an idle box SWIM still declares it in ~5 s because the heartbeat row goes stale at the same time.

Split worlds are in the stand too: every world gets cell `(1,0)` hosted on the node after its home,
one simulator per side leases its cell and 20 persistent entities keep crossing the border
(`x` 90 ↔ 110); after each incident the stand counts entities through every node's cell views and a
run fails on a single lost one. Two 110 s runs (3 nodes, 12 clients, 6 worlds × 20 entities, SIGKILL
of a leader with 2 worlds + a drain, `--kill-every-secs 40`): 0 lost entities in both, worlds re-homed
in 7.6–7.8 s, matches in 12.5 s, all cell views consistent again 13.6 s after the kill. Getting there
fixed: journal chunks replayed out of tick order (a late Despawn erased a returned entity), a node
restarting with the same id resuming its cells from memory while the home re-homed them (two owners),
cell hosts resolved from a local cache that never learns about an unassignment (redirect loops), and
an adopter treating a draining node as a live host by stale cache (its fresh snapshot skipped).
A node now counts as dead only when SWIM says so *and* its database heartbeat is stale, so a fast
same-identity restart is never mistaken for a death.

The stand runs nightly in GitHub Actions (`.github/workflows/soak.yml`, also on demand with a
chosen duration and mode); the JSON report and the nodes' logs are kept as the run's artifact, and
the job fails on any lost write or unsettled incident.

The stand already paid for itself: its first runs showed that a node restarted with the same identity
after a crash was never seen alive again by the survivors (a SWIM "dead" verdict was terminal per node
id); that is fixed and covered by `killed_node_rejoins_with_same_identity`.

