# Musubi

Open-source game backend in Rust: accounts, data, social, chat, leaderboards, matchmaking,
authoritative realtime, dedicated-server fleets and **server meshing** — one Apache-2.0 binary,
clustered out of the box.

Status: alpha — the backend roadmap v0 is implemented and tested (CI runs the full suite against
Postgres); client SDKs and the admin console are not written yet, so integrating today means
talking to the HTTP/WS/QUIC API directly (`GET /openapi.json`).

## Quick start (dev)

```sh
docker run -d --name musubi-pg -e POSTGRES_USER=musubi -e POSTGRES_PASSWORD=musubi \
  -e POSTGRES_DB=musubi -p 5432:5432 postgres:16-alpine
cargo run -p musubi-cli -- dev            # all roles in one process, migrates on start
curl -s localhost:7350/v1/auth/device -H 'content-type: application/json' \
  -d '{"id":"my-device-id-0001"}'          # → access_token / refresh_token
```

Configuration: TOML/YAML file via `--config`, every key overridable with
`MUSUBI__SECTION__KEY` (e.g. `MUSUBI__DATABASE__URL`). `musubi config` prints the effective config.

## Operating it

| command | what it does |
|---|---|
| `musubi doctor` | config sanity (built-in secrets are refused outside `musubi dev`), database + pending migrations, data dir, ports; exit 1 if the server would not start |
| `musubi migrate` | apply migrations and exit |
| `musubi export --out dump.jsonl` / `musubi import --input dump.jsonl [--replace]` | move game data between environments (every `musubi` table except runtime state, foreign-key order, idempotent import) |
| `musubi bench --url … --clients 50 --duration-secs 10 --scenario http\|storage\|ws\|all` | load generator with p50/p95/p99, throughput, errors and 429s |
| `musubi config --format json` | effective configuration (secrets included — do not paste it anywhere) |

Server-to-server keys: `http.server_key` is the bootstrap admin key; issue scoped keys with
`POST /v1/admin/keys {"name","role":"server"|"admin"}` and revoke them with `DELETE /v1/admin/keys/{id}`.
Clean-up of events/deliveries/audit/notifications/sessions runs on the `retention` lease
(`[retention]`, days per table). Nodes drain on SIGTERM or `POST /v1/admin/cluster/nodes/{id}/drain`.

### Measured (not promised)

`musubi bench` on a laptop-class VM, **debug** build, Postgres 16 in Docker, one node, 12 clients,
3 s: HTTP `GET /v1/account` p50 2.5 ms / p99 6.7 ms, storage write/read p50 2.4 ms / p99 6.8 ms,
WebSocket ping p50 0.4 ms / p99 2.6 ms — bounded by the default per-account `[limits]`, not by
the server. Run it against your own deployment; numbers from a release build on real hardware are
the ones that matter.

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
of magnitude faster. Open item: in two of roughly twenty kills on this VM the re-home did not settle
within the 60 s budget; the stand now dumps Raft/world/member state of the survivors when that
happens so it can be chased down.

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

## HTTP API (so far)

| route | auth | purpose |
|---|---|---|
| `GET /healthz`, `GET /readyz`, `GET /v1/node` | — | liveness, DB readiness, node info |
| `POST /v1/auth/device`, `POST /v1/auth/email` | — | authenticate / create account, returns JWT access + refresh |
| `POST /v1/auth/refresh` | — | rotate refresh token (reuse revokes the family) |
| `POST /v1/auth/logout`, `GET /v1/account/me` | bearer | |
| `GET\|PATCH\|DELETE /v1/account` | bearer | own account: profile fields, metadata, identities (masked), soft delete |
| `POST /v1/account/link/{device,email}`, `DELETE /v1/account/unlink/{device,email}` | bearer | link/unlink identities (the last one cannot be unlinked) |
| `GET /v1/users?ids=…&usernames=…` | bearer | public profiles, ≤100 per call |
| `GET\|POST\|DELETE /v1/friends`, `POST /v1/friends/block` | bearer | friends graph: invite/accept/remove/block, Nakama states 0–3, cursor paging, ≤1000 friends |
| `GET\|POST /v1/groups`, `GET\|PATCH\|DELETE /v1/groups/{id}`, `POST /v1/groups/{id}/{join,leave,add,kick,promote,demote}`, `GET /v1/groups/{id}/members`, `GET /v1/users/{id}/groups` | bearer | groups/clans: open or request-to-join, ranks 0 superadmin/1 admin/2 member/3 request, max_count, prefix search |
| `GET\|DELETE /v1/notifications` | bearer | persistent notifications (friend request/accept are emitted automatically), live push over the socket |
| `GET /v1/channel/{id}/messages` | bearer | chat history (direct/group persisted; rooms are ephemeral); live chat over the socket: `channel_join/leave`, `channel_message_send/update/remove`, presence events |
| `POST /v1/channel/{id}/report` | bearer | report a participant (`account_id`, optional `message_id`, `reason`); the channel's last 20 messages are attached as context. Moderation layer (server key): `GET/PUT /v1/admin/chat/filters`, `DELETE …/filters/{id}` — word filters per language (`lang` or all), plain words matched after leet-speak/separator normalisation (`5h.1t` → `shit`) or `regex`, `action` `mask` (word → `****`) or `block` (send rejected); `GET/PUT /v1/admin/chat/{id}/settings` `{slow_mode_secs}` — one message per window per sender; moderation kind `shadow` next to `mute`/`ban` — the sender gets ack and echo, nobody else gets the message; `GET /v1/admin/chat/reports?status=open`, `GET/POST …/reports/{id}` `{status: resolved|dismissed|open, by}` |
| `GET /v1/channel/{id}/messages/{message_id}/replies` | bearer | threads: `channel_message_send` takes `reply_to` (a top-level message of the same channel; one level deep), replies list oldest-first with a cursor; messages carry `reply_to` and `mentions` — `@name` in any string of the content is resolved to accounts server-side (case-insensitive, unknown names ignored) and each mentioned account gets a persistent notification `mention` (code 10) with channel/message/content. Unread: `GET /v1/channel/unread?channels=a,b` → per channel `count` (messages after the read cursor not sent by you) and `mentions`; `POST /v1/channel/{id}/read {message_id?}` moves the cursor forward (never back; omit the id to read up to now) |
| `GET /v1/leaderboards[/{id}]`, `GET\|POST\|DELETE /v1/leaderboards/{id}/records`, `GET /v1/leaderboards/{id}/around/{owner}` | bearer | leaderboards: best/set/incr/decr operators, asc/desc, cron reset windows, 1-based ranks, owner lookup, around-owner paging |
| `POST /v1/server/leaderboards`, `DELETE /v1/server/leaderboards/{id}`, `POST\|DELETE /v1/server/leaderboards/{id}/records/{owner}` | `X-Musubi-Server-Key` | server-to-server: create/delete boards, authoritative writes with operator override |
| `GET /v1/tournaments[/{id}]`, `POST /v1/tournaments/{id}/join`, `GET\|POST /v1/tournaments/{id}/records`, `GET /v1/tournaments/{id}/around/{owner}`; server: `POST /v1/server/tournaments`, `DELETE …/{id}`, `POST …/{id}/records/{owner}` | bearer / server key | tournaments: start/end, active duration per reset window, max_size, max_num_score, join_required, category range listing |
| socket `matchmaker_add` / `matchmaker_remove` / `matchmaker_ready` → `matchmaker_ticket`, `matchmaker_ready_check`, `matchmaker_matched`, `matchmaker_ready_failed` | token | matchmaker: mutual queries (`+field:value`, `-field:value`, `>=`, `[a TO b]`, `*`), min/max count, count_multiple, 15 s wait for max before settling for min; named queues (`queue` property) with MMR and ping windows that widen with wait, fixed teams balanced by MMR, role quotas, cross-play groups, avoid list, no-repeat opponents, match quality grade, `matchmaker_score`/`matchmaker_propose` WASM overrides; ready-check (`matchmaker.ready_check_secs`, per queue) — confirmed tickets return to the queue with their wait, dodgers are kept out for `dodge_penalty_secs` doubling per repeat within 24 h (≤8×) |
| socket `party_create/join/leave/promote/accept/remove/close/join_request_list/data_send/matchmaker_add/matchmaker_remove` → `party`, `party_join_request`, `party_leader`, `party_presence_event`, `party_data`, `party_matchmaker_ticket`, `party_close` | token | parties: open or request-to-join, leader moderation, leader hand-off on leave, party-wide matchmaking tickets |
| socket `match_create`, `match_join` (by id or matchmaker token), `match_leave`, `match_data_send` → `match`, `match_data`, `match_presence_event` | token | relayed (client-hosted) matches, ≤64 players, targeted or broadcast data; authoritative matches come with the actor runtime |
| `GET /v1/matches?label=&min_size=&max_size=`; server: `POST /v1/server/matches {module, params}`, `GET\|DELETE /v1/server/matches/{id}`; socket `match_join/leave/data_send` on an authoritative id | bearer / server key | authoritative matches as actors (`musubi-actor`): handler over an opaque state blob (init/join_attempt/join/leave/tick/terminate), tick 1–60 Hz, label ≤ 2 KB; built-in `echo` module; snapshots in `musubi.match_snapshots` on every membership change / every 50 ticks, claimed atomically and resumed on startup and whenever a node stops heartbeating (failover); match modules in WebAssembly: exports `match_<module>_{init,join_attempt,join,leave,tick,terminate}` (JSON in/out, state as base64) are registered automatically |
| `POST /v1/rpc/{id}`, `GET /v1/rpc`; socket `rpc` | bearer or server key | server logic: sandboxed WebAssembly modules from `runtime.modules_dir` (fuel, memory cap, wall-clock deadline, hot reload per module); ABI v0 documented in `musubi-runtime`; hooks `before_rpc_<id>` / `after_rpc_<id>` / `hook_authenticated` |
| `GET /v1/server/cluster/nodes` | server key | cluster v0: node registry with heartbeats + Postgres LISTEN/NOTIFY bus; chat, notifications, status/presence events and matchmaker tickets are replicated across nodes (the lowest live node id runs matchmaking passes); parties and relayed matches live on their home node and members on any node reach them over the bus (matchmaker-formed relayed matches are homed on the leader) (gossip/Raft/QUIC bus replace this behind the same API) |
| `GET\|POST /v1/server/fleets`, `GET\|DELETE /v1/server/fleets/{id}`, `POST …/{id}/scale\|allocate`, `GET …/{id}/servers`, `GET\|DELETE /v1/server/gameservers/{id}`; SDK (server key + `X-Musubi-Server-Id`): `GET /v1/gameserver`, `POST /v1/gameserver/{ready,health,allocate,shutdown,label,connect}` | server key | dedicated-server orchestration v0 (`musubi-fleet`): fleets with buffer/max/port range, reconciler every 5 s (reaps dead/unhealthy, tops up to buffer), `local` process provider and `docker` provider (Docker CLI, `spec.image`, port published UDP+TCP), env `MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY`, group connect tokens; tickets with string property `fleet=<name>` get a server allocated and delivered in `matchmaker_matched.server`; kubernetes/agones/edgegap providers next |
| `GET\|PUT /v1/server/matchmaker/queues`, `DELETE …/queues/{name}`, `GET …/stats` | server key | matchmaker queues at runtime (boot-time ones come from `[[matchmaker.queues]]`), waiting tickets per queue, open matches; Prometheus `musubi_matchmaker_wait_seconds{queue,region}`, `…_match_quality`, `…_rejections_total{reason}` |
| `POST /v1/tournaments/{id}/join {code}` | token | private tournaments: `join_code` at creation makes `join` require the code (`private: true` in listings, the code is never returned; scores without an explicit join are refused). Every finished window of a board/tournament with a reset schedule fires `hook_leaderboard_reset` / `hook_tournament_end` once (`{id, expiry, size, records}` with final ranks) for rewards — idempotent across nodes and restarts |
| `GET /v1/leaderboards/{id}/tier`, `POST /v1/server/leaderboards/{id}/rollover` | token / server key | bucketed leaderboards (`bucket_size`: players compete in buckets of K assigned on their first score of a window — ranks, pages and `around` stay inside the bucket) and leagues (`league {tiers, promote_pct, demote_pct}` on a board with a reset schedule: at every reset the top/bottom share of each tier-bucket moves one tier up/down; `rollover` closes a window by hand, e.g. to end a season early) |
| `GET /v1/rating/{queue}`, `GET …/top`, `GET …/{account}`; `GET\|PUT /v1/server/rating/queues`, `GET\|DELETE …/{id}`, `POST …/{id}/results\|season\|decay` | token / server key | skill rating per queue: Elo, Glicko-2 or OpenSkill (Weng-Lin), team games with ranks/draws, placement games, seasons with soft reset (`carry`), inactivity decay (Elo loses points to a floor, Glicko-2/OpenSkill widen uncertainty), one 1500-centred `display` scale across systems; a matchmaker queue of the same name gets `mmr` from it |
| `POST /v1/storage/{read,write,delete}`, `GET /v1/storage/{collection}` | bearer | legacy KV storage, Nakama permission codes, CAS versions |
| `GET /ws?token=…&format=json\|binary` | token | realtime socket: ping, status/presence follow, rpc (envelope `{cid, msg}`, JSON or postcard) |
| QUIC `quic.listen` (UDP) | access JWT in the first frame `{token, format, status}` | same envelopes/encodings as WebSocket over one bidirectional quinn stream (`type:u8 \| len:u32be \| payload`), ALPN `musubi-rt/1`, PEM cert or self-signed dev cert; `match_data` with `reliable=false` travels as QUIC datagrams (clients may send datagrams too) |

## Layout

| crate | role |
|---|---|
| `musubi-core` | shared types, ids, errors, config |
| `musubi-proto` | wire protocol and schemas |
| `musubi-cluster` | membership, raft, placement, leases |
| `musubi-actor` | actor model, journal, snapshots, failover |
| `musubi-transport` | QUIC / WebTransport / WebSocket / UDP channels |
| `musubi-edge` | ingress: TLS, auth, rate limiting, routing |
| `musubi-auth` | identities, providers, sessions |
| `musubi-account` | accounts, profiles, wallets |
| `musubi-storage` | typed collections and legacy KV storage |
| `musubi-server` | node binary (`musubi-server`) |
| `musubi-cli` | developer CLI (`musubi dev`, `musubi deploy`, …) |

## License

Apache-2.0.

## Server meshing (v0 core, `musubi-mesh`)

Replication-layer data model independent of transport/engine: grid cells with simulator leases (`lease_ttl_ticks`), entity-level authority (only the lease holder writes; versioned components), ghosts within `ghost_margin` of a neighbour's border, handover state machine (border + `hysteresis` → `Started` → `handover_ack` by the new owner → `Completed`, or `Cancelled` after `handover_timeout_ticks`; inputs during transit are buffered for the new owner), interest subscriptions with per-client deltas that never see ownership changes, persistent-entity snapshots/restore, orphaned-cell detection when a lease lapses. Wired into the server: named worlds ticked at 30 Hz (`realtime/world.rs`); simulator API with the server key — `GET|POST /v1/server/worlds`, `GET|DELETE /v1/server/worlds/{w}`, `POST …/{w}/lease|renew|spawn|despawn|apply|handover/ack`, `GET …/{w}/view/{x}/{y}`, `GET …/{w}/events?sim=` (handover requests/results, routed inputs); clients use `world_subscribe {world,x,y,radius}` / `world_unsubscribe` / `world_input {world,entity,input}` and receive `world_delta {spawned,updated,despawned}`. Shard controller v0 (`musubi-mesh::controller`): simulators register with a capacity (`POST|DELETE /v1/server/worlds/{w}/sims {sim, capacity}`), unowned cells are leased on demand (spawn or client interest; adjacency preferred, then lowest load), a pass every 30 ticks re-leases orphaned cells to live simulators and sheds cells off simulators above `split_cost`; simulators learn about it through `cell_assigned`/`cell_revoked` events. Simulator SDK (`musubi-sim-sdk`): `WorldClient` over the HTTP API plus `run_sim(client, sim, SimConfig, impl Simulator, stop)` — registers, heartbeats, drains events, auto-acks handovers (unless refused), ticks owned cells and pushes `Update`s; two SDK simulators handing an entity over is covered by a test. Worlds are homed on one node (directory `musubi.worlds`): clients on any node subscribe/send inputs through the bus and get deltas back, simulators hitting another node get a 307 to the home's `advertise` address (409 with the home node id if none). Multi-home replication of one world and engine SDKs (Unreal headless first) are next.

Streaming simulator protocol: `GET /v1/server/worlds/{w}/stream?sim=<uuid>` (server key) upgrades to a WebSocket per node hosting the simulator's cells. After every registry tick the node renews the simulator's lease (connected = alive) and pushes one `{"type":"tick","tick":n,"cells":[{cell, owned, ghosts}],"events":[…]}` frame, replacing per-tick polling of `/view` and `/events`. Commands are JSON objects with `op` ∈ `lease | spawn | despawn | apply | handover_ack` and the fields of the matching HTTP body (no `sim`); replies echo `op` and an optional `id`, and a cell hosted elsewhere answers with `redirect` (that node's URL) exactly like the HTTP endpoints. The soak stand keeps one stream per split world open across kill/drain (`stream: tick frames / reconnects`).

Cross-boundary interactions: a simulator that hits a ghost (an entity owned by another cell or node) calls `POST /v1/server/worlds/{w}/interact {sim, entity, kind, payload}` (or the stream op `interact`) instead of writing the target. The replication layer delivers an `interaction` event (`kind`, `payload`, `from_sim`, `seq`, `tick`) to the simulator that owns the target, which alone applies the effect — one winner per conflict, in per-entity sequence order (the owner node is the single point of order; `seq` restarts when the entity changes owner). A target in transit receives it with its buffered inputs after the handover; a target hosted on another node is forwarded over the bus.

Interest v2: a client's `world_subscribe` carries an optional `budget_bytes` per tick (default `client_budget_bytes` = 16 KB in the world config) and an optional `filter` (component → value every delivered entity must match) on top of the radius. Each tick the entities in range are ranked by `relevance × (1 + age)` — relevance falls with distance, age is the ticks since the client last heard about the entity — and sent in that order until the estimated bytes exceed the budget (at least one always goes); the rest keep their old state and come back later with a higher priority, so a crowded radius degrades to lower update rates for far entities instead of bursts. Despawns are always delivered. Positions are quantized to `pos_quantum` (0.01) and jitter below it is not an update.

Degradation under load (plan 4.8): `soft_cap` in the world config refuses spawns into a cell that already holds that many entities (`429`, event `cell_full` to the simulator; handovers always pass — physics cannot wait at a border). A spawn with `queue: true` (HTTP and stream op) waits instead: `202 {queued, ticket, cell, position, eta_ticks}` and event `queued`; each tick the cell admits waiting spawns in arrival order as entities leave (`queue_admitted` with the entity), reports position/ETA changes (`queue_update`, ETA = position / smoothed departures per tick, `null` until the cell has seen departures) and drops tickets older than `queue_ttl_ticks` (600) or whose simulator lost the cell (`queue_expired` with `timeout` / `lease_lost`). Cell cost (plan 4.4 dynamics): `cost_entity`·entities + `cost_player`·clients whose interest is centred in the cell + `cost_traffic`·entities in handover out of it (smoothed per tick), plus `cost_lag` per tick the owning simulator lags; a simulator over `split_cost` sheds its least-coupled cell to one that stays under the bar, and one under `merge_cost` for `merge_after_rounds` control rounds (30 ≈ 30 s) hands every cell to the owners of neighbouring cells that stay under ¾ of `split_cost` — all or nothing — and is left free (co-locate: ghosts and handovers stop crossing simulators; the hysteresis keeps it from flapping). Split before dilation: a simulator whose reported tick lags the world by more than `tidi_after_ticks` first sheds a cell (the one least coupled to its others) to an alive simulator that keeps up, regardless of `split_cost` (reconcile reason `lagging`); only what it still holds is dilated. Time dilation: a simulator reports the last tick it fully processed (`tick` in `POST /renew`, op `tick_done` in the stream); when it lags the world by more than `tidi_after_ticks` (15) its cells run at `tidi_after_ticks / lag` of real time, not below `tidi_min` (0.1) — the simulator gets a `time_scale` event and sees `time_scale` per cell in its tick frames, clients get `time_scale` in `world_delta` (slowest cell in their radius) to interpolate against; the cell returns to 1.0 as the simulator catches up. Interest shedding is the per-client budget of interest v2.

World persistence (plan 4.9): a spawn may carry `decay_ticks` — the entity disappears after that many ticks without a write, interaction or handover (event `decayed` to the cell's simulator; persistent ones are journaled so no shard resurrects them) — abandoned litter cleans itself up. A cell no client has watched for `hibernate_after_ticks` (1800 ≈ 60 s; 0 = never), leased or holding entities under a lapsed lease, with nothing in transit or written to recently, hibernates: its lease is revoked (`cell_hibernated` with `ephemeral_despawned` / `persistent_kept` to the last simulator), ephemeral entities die with it, persistent ones stay in the shard's memory and snapshot, and the cell is not an orphan — nothing re-leases it while nobody looks. A client's interest reaching it (or a simulator leasing it) wakes it within one control round with its persistent entities in place. Old snapshots and journals load unchanged (the new entity fields default). Time machine: every shard snapshot is also appended to `musubi.world_history` (retention `world_history_days`, 7) and the journal is kept back to the oldest retained history row, so `GET /v1/admin/worlds/{w}/at?tick=T` rebuilds the persistent entities as of any retained tick — newest history snapshot of each shard at or before T plus the journal ops up to T (provenance-aware merge, as in recovery) — and reports the retained range; `POST /v1/admin/worlds/{w}/snapshot` takes one now (shards that left the world contribute their last snapshot only).

Lag compensation: the shard keeps `history_ticks` (15 ≈ 500 ms) of positions per entity. `POST /v1/server/worlds/{w}/rewind {entity, tick, x?, y?, tolerance?}` (and the stream op `rewind`) answers where the entity was at the end of `tick` and whether `(x, y)` was within `tolerance` (1.0) of it — `pos`/`hit` are null when history does not reach that far back, never a guess. An `interact` carrying the actor's `tick` (and `x`, `y`, `tolerance`) reaches the target's owner with the same answer under `rewind`, computed on the owner from its own history whether the interaction came from a local simulator or over the bus — Source-style rewind for hitscan on a ghost across a border. Co-locate (docking, grappling — physics that cannot be split across two owners): `POST /v1/server/worlds/{w}/colocate {sim, entity, with, ttl_ticks}` (stream op `colocate`) pins `with` into `entity`'s cell for `ttl_ticks` (300) — the regular handover brings it over with state and buffered inputs (its owner hears `co_locate`, then `handover_request` reaches the caller, who acks), the caller is authoritative over both while the pin holds, and when it lapses the partner hands back to wherever its position says. A partner living on another node is reached over the bus: the caller pins first (so the arrival lands in its cell), the node hosting the partner pins too and its regular cross-node handover ships the entity; the answer carries `forwarded: true`.

Load: `musubi bench --scenario world --clients N --worlds W --entities E --radius R` creates W arenas with E entities each moved by a simulator every tick, spreads N WebSocket players over them and reports frames per player per second, inter-frame gap p50/p99, bytes and entity updates per second, plus the server's own `musubi_world_tick_duration_seconds` (simulation step) and `musubi_world_fanout_duration_seconds` (per-player delta build and send) from `/metrics`. Players walk their interest circle in short random hops (5 Hz), as a moving player would; arenas are deleted when the run ends. Every world ticks on its own thread with its own clock (a slow world never holds back the others); a player whose view did not move re-examines only the entities that changed since its last delta (per-cell change ticks plus a change log), deltas are built sequentially on the world's own thread, component values travel as JSON text serialized once per change, and `--max-hz N` caps deltas per player per second (skipped ticks fold into the next delta). Reference on an 8-core dev box, `--wire binary`: 1 000 players over 4 worlds × 500 entities → 30.1 frames/player/s (the full 30 Hz), gap p50 33 ms / p99 47 ms, 0 errors, ≈0.8 cores. 10 000 players over 20 worlds × 500 entities on the same box: every player connects (the handshake no longer reads the account row — the username rides in the token — and the listen backlog is 4096) and the box is the limit: in the steady window the server takes 6.3 of the 8 cores (half of it the twenty tick threads, half the runtime workers) and the load generator the rest, the simulator (clock-paced, see `sim_ticks_per_sec`) reaches 21.2 Hz and players get 21.4 frames/s each, 0 errors, 1.99 M entity updates/s at 138 MB/s, `apply` ≈35 ms (runtime starved, not the world lock). Found and fixed on the way: glibc malloc's arena locks (most runtime-worker samples sat in realloc/free waiting for one; the server binary uses mimalloc), the world mutex taken by every subscription and `home_of` on a session task (interest is now queued and applied by the world's tick thread, the home lookup reads a mirror), the tick holding the lock while building deltas (deltas are built from a `WorldView` snapshot off the lock), deltas that cloned every component's JSON value per client and re-serialized it to estimate its size (they carry shared text now), and a benchmark that paced its simulator by the server's reply and so under-reported frames. What remains is CPU per frame — about 30 µs across build, encode and write — so 30 Hz for 10 000 players on one node of this size is still open; the next lever is sharing the serialized component text between clients instead of cloning it per frame.

Multi-region placement: `POST /v1/server/worlds` takes `region`; a world for another region with a live node there is created as an unowned row (`202 {pending: true}`) that a node of that region adopts on its next failover round (≤ 0.5 s), and failover itself prefers nodes of the world's region — a node elsewhere adopts only when none of them is alive (`musubi.worlds.region`, migration 0049). Without a live node in the asked region the world is simply placed on the creating node.

Operators: `GET /v1/admin/worlds/{w}/entities` dumps this node's shard of a world — every entity with cell, position, transit state and the live leases (where is entity X right now?); `musubi soak --rejoin-delay-secs N` keeps a killed node down for N seconds so the survivors alone must account for every entity.

Shard rows are merged with provenance: when a world is rebuilt from several nodes' snapshots and journals (home hand-off at drain, a host that died, a host that handed its cell back), a `Despawn`/`Apply` from node X only touches entities whose current copy came from X — a stale "it left my shard" record of a former host can no longer erase the fresh home snapshot. Hand-offs also stop a node from persisting stale rows afterwards: a drained home drops its worlds right after the hand-off snapshot, a shard that gave up its last cell (drain, reassignment, boot hand-back after a restart) is dropped with its journal buffer, and cell assignments consulted during adoption and pruning are confirmed through the Raft leader. Found by the soak with `--victim leader` and a 1 s rejoin: 1 in 2–3 runs lost 5–15 persistent entities at drain; three consecutive runs since are at 0.

## Integrations (Zanshin LiveOps, Aurix voice)

`[zanshin] url, server_key, flush_secs` and `[aurix] url, api_key, party_channel_type, match_channel_type` in the config (unset `url` = disabled). The Musubi account id is the `external_id` on both sides.

- Zanshin: authentication is tracked as `musubi.authenticated` (batched `POST /server/v1/events`); clients evaluate flags / live events for themselves via `GET /v1/liveops/flags?names=` and `GET /v1/liveops/live-events` (proxied with the server key, never exposed).
- Aurix: `POST /v1/voice/token {party_id | match_id, display_name?, speak?}` checks membership on this node and mints a player token for the ad-hoc channel `party-<id>` / `match-<id>` (created on first join, removed when empty); the response carries Aurix's `token`, `user_id`, `expires_at`, `channel_id`, `endpoint`.

## Economy v0 (`musubi-economy`)

Currencies with optional caps; wallets with an append-only ledger (`delta`, `balance_after`, `reason`, `ref`, idempotency key — a replayed key returns the original entry, balances never go negative); item catalog (stackable with `max_stack` or unique instances with `instance` JSON); store listings priced in several currencies with `max_per_account`, purchase = debit + grant in one transaction, idempotent. Players: `GET /v1/wallet`, `GET /v1/wallet/ledger?currency=&limit=`, `GET /v1/inventory`, `POST /v1/inventory/consume {id, quantity}`, `GET /v1/store`, `POST /v1/store/buy {store_item_id, currency, idempotency_key?}`. Server key: `GET|POST /v1/server/currencies`, `POST /v1/server/wallet/adjust {account_id, currency, delta, reason, ref, idempotency_key, metadata}`, `GET|POST /v1/server/items`, `POST /v1/server/inventory/grant`, `GET|POST /v1/server/store`. Rewards tables, progression, battle pass, auctions and IAP validation are next.
