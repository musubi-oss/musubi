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
| `POST /v1/auth/device`, `POST /v1/auth/email` | — | authenticate / create account, returns JWT access + refresh. Brute force (5.1): failed email logins per address → exponential lockout (`[session] lockout_threshold/base_secs/max_secs`); after `captcha_after` (3) failures the request must carry `captcha` — verified by a `hook_auth_captcha` module (`{token, ip}` → `{ok}`) or by `captcha_verify_url` + `captcha_secret` (Turnstile / hCaptcha / reCAPTCHA siteverify), otherwise `403 captcha_required` / `captcha_invalid`. Every login stamps the client IP on its session; a login from an address the account was never seen at emits `account.suspicious_login` on the event bus and `hook_suspicious_login` to modules |
| `POST /v1/auth/refresh` | — | rotate refresh token (reuse revokes the family) |
| `POST /v1/auth/logout`, `GET /v1/account/me` | bearer | |
| `GET\|PATCH\|DELETE /v1/account` | bearer | own account: profile fields, metadata, identities (masked), soft delete |
| `POST /v1/account/link/{device,email}`, `DELETE /v1/account/unlink/{device,email}` | bearer | link/unlink identities (the last one cannot be unlinked) |
| `GET /v1/users?ids=…&usernames=…` | bearer | public profiles, ≤100 per call |
| `GET\|POST\|DELETE /v1/friends`, `POST /v1/friends/block` | bearer | friends graph: invite/accept/remove/block, Nakama states 0–3, cursor paging, ≤1000 friends |
| `GET\|POST /v1/groups`, `GET\|PATCH\|DELETE /v1/groups/{id}`, `POST /v1/groups/{id}/{join,leave,add,kick,promote,demote}`, `GET /v1/groups/{id}/members`, `GET /v1/users/{id}/groups` | bearer | groups/clans: open or request-to-join, ranks 0 superadmin/1 admin/2 member/3 request, max_count, prefix search |
| `GET\|DELETE /v1/notifications` | bearer | persistent notifications (friend request/accept are emitted automatically), live push over the socket |
| `GET /v1/channel/{id}/messages` | bearer | chat history (direct/group persisted; rooms are ephemeral); live chat over the socket: `channel_join/leave`, `channel_message_send/update/remove`, presence events |
| `POST /v1/channel/{id}/report` | bearer | report a participant (`account_id`, optional `message_id`, `reason`); the channel's last 20 messages are attached as context. Moderation layer (server key): `GET/PUT /v1/admin/chat/filters`, `DELETE …/filters/{id}` — word filters per language (`lang` or all), plain words matched after leet-speak/separator normalisation (`5h.1t` → `shit`) or `regex`, `action` `mask` (word → `****`) or `block` (send rejected); `GET/PUT /v1/admin/chat/{id}/settings` `{slow_mode_secs}` — one message per window per sender (the window lives in `chat_slow_mode` and is advanced by one conditional upsert, so it is shared by every node); moderation kind `shadow` next to `mute`/`ban` — the sender gets ack and echo, nobody else gets the message; `GET /v1/admin/chat/reports?status=open`, `GET/POST …/reports/{id}` `{status: resolved|dismissed|open, by}` |
| `POST /v1/reports` {target_id, category, details?, evidence?}, `GET /v1/reports`; `GET /v1/admin/reports?status&target_id&category&cursor`, `GET /v1/admin/reports/{id}`, `POST /v1/admin/reports/{id}` {status, decision?, by?, ban?}, `GET /v1/admin/accounts/{id}/reports` | bearer / server key | player reports (5.12): categories `cheating, harassment, griefing, offensive_name, spam, exploit, other`, free text ≤ 2000 chars and an `evidence` object (match_id, world + tick, channel_id + message_id, url — whatever the client has); one open report per reporter → target, 20 per reporter per day, no self-reports; `player.reported` on the bus with the target's open-report count. Moderators list the queue (status/target/category filters, cursor), read one, see a per-account summary (distinct reporters, counts by category/status, recent) and decide: `reviewing`, `dismissed` or `actioned` — with `ban: {until_secs?, shadow?, reason?, message?}` the target is banned exactly like `/v1/admin/accounts/{id}/ban` and every other open report on them is closed as actioned (`report.decided` on the bus). Reporters see the outcome, not moderator notes |
| `GET /v1/channel/{id}/messages/{message_id}/replies` | bearer | threads: `channel_message_send` takes `reply_to` (a top-level message of the same channel; one level deep), replies list oldest-first with a cursor; messages carry `reply_to` and `mentions` — `@name` in any string of the content is resolved to accounts server-side (case-insensitive, unknown names ignored) and each mentioned account gets a persistent notification `mention` (code 10) with channel/message/content. Unread: `GET /v1/channel/unread?channels=a,b` → per channel `count` (messages after the read cursor not sent by you) and `mentions`; `POST /v1/channel/{id}/read {message_id?}` moves the cursor forward (never back; omit the id to read up to now) |
| LiveOps feed (`[zanshin]`) | — | every account-bound event on the event bus (`account.authenticated`, `friend.added`, `purchase.*`, `match.ended`, `group.*`, `lobby.*`, …) is also tracked in Zanshin with the account id as `external_id`, batched by the existing flusher — the same stream webhooks get; WASM modules read LiveOps through `host_call` ops `liveops.flags {names}` and `liveops.live_events` (per-account answers cached 30 s on the node) |
| `POST /v1/admin/webhooks/{id}/deliveries/{event_id}/redeliver` | server key | put a delivery back in the queue — one marked `dead` after 12 attempts, or a delivered one the receiver lost: attempts and backoff start over and the next dispatch cycle sends it again (unknown pair → 404) |
| `POST /v1/account/mfa/totp`, `POST …/totp/verify`, `DELETE …/totp`, `POST /v1/auth/mfa` | bearer / public | TOTP second factor (5.1): enrol → base32 secret, `otpauth://` URI for the QR code and 8 one-time recovery codes (stored hashed); nothing is enforced until a code confirms the enrolment. With an authenticator enabled, `POST /v1/auth/email` answers `{mfa_required, mfa_token}` instead of a session — the 5-minute token is not accepted as a bearer anywhere — and `POST /v1/auth/mfa {mfa_token, code}` finishes the login (a recovery code works once); RFC 6238 SHA-1 / 30 s / 6 digits with ±1 step of skew. Device and social logins are untouched (the device or the identity provider is the second factor there); refresh is untouched |
| `GET/POST /v1/server/fleets/{id}/builds`, `POST …/builds/{v}/promote`, `DELETE …/builds/{v}`, `GET …/fleets/{id}/usage?from&to` | server key | fleet build pipeline (5.9): a build is a versioned `spec` (image / command / env / `vcpu`); `canary_percent` > 0 registers it as a canary that takes that share of newly started servers (optionally only in `canary_regions`), 0 makes it active at once and retires the previous build; `promote` makes any build active (a canary, or an old one — that is the rollback) and retires the rest; the active build cannot be retired; running servers are never touched — new ones pick the build at start and record `build_version`. Usage: vCPU-hours of the fleet's servers overlapping the window, per build (start → termination or now, × `vcpu`, default 1), with `ready_at` / `terminated_at` now kept on every server |
| `POST /v1/server/liveops/changed` {any body} | server key | LiveOps change push (5.12): point Zanshin's flag/live-event change webhook here — cached answers (shared by `GET /v1/liveops/*` and the module host ops `liveops.flags` / `liveops.live_events`, 30 s TTL) are dropped on every node over the bus and modules get `hook_liveops_changed` with the webhook body, so a flag flip reaches running code at once instead of at the next TTL |\n| `GET /v1/gameserver/watch?since&timeout_secs`, `POST /v1/gameserver/players` {players, capacity}; `POST /v1/server/fleets/{id}/allocate` {payload, backfill} | server key + X-Musubi-Server-Id | game-server SDK depth (5.9): WatchGameServer as a long-poll on the server's change counter (`version`, bumped by every state/label/allocation/occupancy change — returns at once with `changed: true`, or after the timeout with the unchanged record), occupancy reporting, and backfill allocations that put the match onto a live `allocated`/`in_game` server with free slots (same connect token, `backfill: true` in the reply) before starting a fresh one |
| `POST /v1/server/store` {…, available_from, available_until, bundle, min_level, level_track, discount_percent, discount_until, rotation, rotation_slots}, `GET/PUT /v1/server/store/coupons`, `DELETE …/{code}`; `POST /v1/store/buy` {…, coupon} | server key / bearer | store depth (5.11): availability windows (hidden and unbuyable outside), bundles (extra items granted with the listing), level gates against a progression track (shown as `locked: "level"`), timed discounts (`effective_prices` for the viewer), coupons (percent, restricted to a listing, use count, once per account, expiry — applied after the discount, use recorded on the purchase) and seeded daily rotations (`rotation_slots` of a `rotation` group on sale each UTC day, chosen by FNV-1a of group/day/id so every node agrees) |
| `GET`/`PUT /v1/server/entitlements` {provider, external_id, reward_table, label?}, `DELETE …/{provider}/{external_id}`; `POST /v1/entitlements/sync` {provider}; `GET /v1/entitlements`; `POST /v1/server/entitlements/grant` / `revoke` {account_id, provider, external_id} | server key / bearer | platform entitlements (5.11): a platform purchase (Steam DLC appid, PSN entitlement label, Xbox product id, …) maps to a reward table and is paid to each account at most once (`entitlement_grants` is the idempotency key, `entitlement.granted` / `entitlement.revoked` on the bus). `sync {provider: "steam"}` checks every Steam catalog entry for the caller's SteamID with the publisher Web API (`ISteamUser/CheckAppOwnership`, `[iap.steam] url/key`): newly owned → granted, no longer owned (refund) → revoked. Console stores expose entitlements only to the title, so the game server reports them through the trusted grant/revoke routes. Revoke marks the grant; delivered items are not clawed back automatically |
| `GET`/`PUT /v1/server/grant_limits` {kind, key, window_secs, max_per_account}, `DELETE …/{kind}/{key}`; `GET /v1/admin/accounts/{id}/grants` | server key | limited grants (5.11): a per-account cap on server-side grants of a `currency` (sum of positive deltas), an `item` (quantity) or a `reward_table` (grants) inside a sliding window — `POST /v1/server/wallet/adjust`, `/v1/server/inventory/grant`, `/v1/server/rewards/grant` and the module host ops `wallet.adjust` / `inventory.grant` all answer `429 resource_exhausted` once the cap would be exceeded; a backstop against a buggy module or a leaked key duplicating rewards. Usage is only recorded for limited keys; the admin route lists an account's recent limited grants |
| `POST /v1/server/matches/{id}/results` {teams:[{rank?, players:[{account_id, stats?}]}], queue?, rewards?: {"1": table, "default": table}, quest_event?: {name, delta?}, progression?: {track_id, xp_by_rank: {"1": n, "default": n}}, metadata?}; `GET` same path | server key | end-game report (5.7): one call from the game server fans the result out — rating update on `queue` (same math as `/v1/server/rating/queues/{id}/results`), a reward table per rank for every player (under grant limits, reference `match:<id>`), a quest event per player, progression XP per rank; `match.results` on the bus and `hook_match_results` to modules. Idempotent per match id: the first report wins, repeats (or `GET`) return the stored outcome with `replayed: true`. Per-step failures are recorded per player (`rewards_error`, …) instead of failing the whole report |
| `POST /v1/admin/accounts/{id}/ban` {reason, message, until_secs, shadow}, `GET …/ban`, `POST …/unban` | server key | account bans (5.2): bans with reason, a player-visible message and an expiry — the player sees `account banned: <message> (until …)` at login and on every request, the leader lifts expired bans once a minute; `shadow: true` keeps the account working but its chat reaches nobody and its matchmaking tickets never match (cluster-wide, replicated like hard bans); soft-deleted accounts are purged for good after `[retention] deleted_account_days` (30, 0 = never) with an `account.purged` event |
| `PATCH /v1/admin/accounts/{id} {username?, display_name?, avatar_url?, lang?, timezone?, location?, metadata?}` | server key | console edit of a player profile with the same validation as the player's own `PATCH /v1/account` (taken usernames → 409); returns the account, records `account.updated` on the bus and in the audit log |
| `GET /v1/admin/accounts?identity=provider:subject` | server key | find a player by a linked identity — `device:<id>`, `email:<address>`, `steam:<id>`, `google:…`, `apple:…`, `oidc:<name>:<sub>`, `passkey:<credential id>` (the last `:` splits provider from subject); combines with `q`, `include_deleted`, `cursor` |
| `POST /v1/account/passkeys/options` → `POST /v1/account/passkeys` {token, name, credential}, `GET/DELETE /v1/account/passkeys[/{id}]`; `POST /v1/auth/passkey/options` → `POST /v1/auth/passkey` {token, credential} | bearer / none | passkeys (5.1): WebAuthn on `ring` (no OpenSSL) — registration stores the COSE key (ES256 or RS256, attestation `none`) as a `passkey` identity, login verifies clientDataJSON type/challenge/origin, rpIdHash, user-presence flag, signature and a non-regressing sign counter (cloned authenticators refused); the challenge travels in a signed 5-minute token no bearer check accepts; `[social.passkeys] rp_id, rp_name, origins` |
| `POST /v1/auth/oidc/{provider}` {id_token}, `POST /v1/account/link/oidc/{provider}`, `DELETE /v1/account/unlink/oidc/{provider}` {subject} | bearer | generic OIDC sign-in (5.1): any OpenID Connect issuer configured under `[social.oidc.<name>] issuer, client_ids, jwks_url?` — Discord, Twitch, Epic, Xbox, Keycloak, …; JWKS comes from `<issuer>/.well-known/openid-configuration` when `jwks_url` is empty (cached 1 h, refreshed on an unknown `kid`), `iss`/`aud`/`exp` checked; identities are `oidc:<name>` and merge/link like any other provider |
| `POST /v1/auth/xbox` {user_hash, token}, `POST /v1/auth/psn` {id_token}, `POST /v1/auth/nintendo` {id_token}; `POST /v1/account/link/{xbox,psn,nintendo}` | — / bearer | console sign-in (5.1). Xbox: the XSTS token for the `http://xboxlive.com` relying party plus its user hash are presented to the profile service (`[social.xbox] url = "https://profile.xboxlive.com"`) as `XBL3.0 x=<uhs>;<token>`; identity `xbox:<xuid>`, gamertag returned on link. PSN / Nintendo: the title's `id_token` is an RS256 JWT — configure `[social.psn]` / `[social.nintendo]` (`jwks_url`, `issuers`, `client_ids`) for the environment from DevNet / NDP; identities `psn:<account id>` (PSN `online_id` returned on link) and `nintendo:<nsa id>`. All three are found by `GET /v1/admin/accounts?identity=…` |
| `POST /v1/auth/queue` {ticket}; `GET`/`DELETE /v1/admin/auth/queue` | — / server key | login queue (5.1): with `[session] login_queue_capacity` > 0, any verified login while that many accounts are online cluster-wide (or others are still waiting) answers `202 {queued, ticket, position, eta_secs, expires_in}` instead of a session; the client polls `/v1/auth/queue` and receives the session the moment its place is reached — arrival order, `login_queue_admit_per_sec` paces the drain, a client silent for `login_queue_ttl_secs` loses its place, tickets last an hour. Admin GET shows capacity/online/waiting, DELETE drops all tickets |
| `rtt {ms}`, `match_host_transfer {match_id, session_id}` → `match.host`, `match_host` | ws | relayed host migration (5.8): every relayed match has a host (creator, or the first joiner of a matchmaker-made match) carried in the `match` snapshot; clients report their measured ping→pong round trip with `rtt` and when the host leaves or drops the member with the lowest reported RTT takes over (never reported = last, ties = join order), everyone gets `match_host`; the host can hand the role over explicitly; cross-node members are handled on the match's home node |
| `GET /v1/matches/{id}/replay` | bearer | match recording (5.8): create the match with `params.record: true` and every snapshot (module state, base64) and journal chunk (processed inputs and membership steps) is kept in `match_recordings` / `match_recording_frames` instead of being pruned with the failover journal; the endpoint returns the header (`started_at`, `ended_at`, `last_tick`, `live`) and frames in tick order while the match runs and after it ends; `DELETE /v1/server/matches/{id}/replay` drops it; `[retention] replay_days` (30, 0 = forever) sweeps old recordings |
| `match_join {spectator: true}` | ws | spectator mode (5.8) for authoritative matches: the session gets the match snapshot and every `match_data` / presence event the match sends, is not counted against `max_players` (joins a full match), never reaches the module's `join_attempt`/`join`/`leave`, and its own `match_data_send` is refused with `PermissionDenied`; leaving is silent for the players |
| `POST /v1/account/merge` | bearer | account merge (5.1, guest → social login): `{access_token}` of the other account proves you own both; in one transaction its identities move over (an email stays behind if you already have one), wallets are summed with `merge` ledger rows, inventory / friends / group memberships / recent players / notifications are unioned, storage objects move unless you already have the same collection/key (yours win); leaderboard records and ratings stay yours; the other account is closed like `delete` and kicked everywhere; `account_merges` keeps the journal with the per-table report that the call returns |
| `GET /v1/account/sessions`, `DELETE /v1/account/sessions/{id}` | bearer | device management (5.1): every live login (one entry per refresh family: device id, user agent, IP, created / last seen / expires, `current` for the caller's own) and sign-out of a single device — its refresh family is revoked so it cannot refresh again (the access token it already holds expires within its 15-minute lifetime); `POST /v1/auth/logout {all_devices: true}` still signs out everything |
| `POST /v1/storage/patch`, `GET /v1/storage/{collection}?sort=`, `GET /v1/storage/{collection}/count` | bearer | storage depth (5.3): `patch` applies a JSON Merge Patch (RFC 7386: nested objects merge, `null` removes a member) to existing objects under the row lock with the same OCC `version` rules as write (missing object → 404); listing takes `sort=field` / `sort=-field` (nested `stats.wins`) ordered by the JSON value with a keyset cursor, and `count` returns how many objects match `where`/`prefix` under the same visibility rules; `[storage] max_objects_per_user` (10 000) / `max_bytes_per_user` (100 MiB) quotas per owner (0 = unlimited, system owner exempt) are kept by a trigger-maintained `storage_usage` table so TTL/retention purges count too — a write or patch that would overflow is rejected and rolled back |
| `POST/DELETE /v1/groups/{id}/ban`, `GET /v1/groups/{id}/bans`, `POST /v1/groups/{id}/announcement`, `GET /v1/groups/{id}/events` | bearer | groups depth: admins ban accounts (`{ids, reason, until_secs?}`; members are kicked, a banned account cannot join or request to join until the ban lapses; nobody of equal/higher rank), lift bans, list active bans; pin an announcement (`{text}`, ≤ 2048, `null` clears — shown on the group as `announcement`/`announcement_at`, every member gets a persistent notification `group_announcement`, code 30); the group journal records `joined`/`join_requested`/`kicked`/`banned`/`unbanned`/`announcement` with actor, subject and payload, newest first with an id cursor, members only |
| `GET/PUT /v1/groups/{id}/roles`, `POST /v1/groups/{id}/rank` | bearer | role hierarchy with permission bitmasks (5.4): ranks 0 (owner, holds everything) .. 7, each with a name and `permissions` bits — INVITE 1, KICK 2, BAN 4, ANNOUNCE 8, EDIT 16, PROMOTE 32, TREASURY_DEPOSIT 64, TREASURY_WITHDRAW 128; defaults: admin (1) = all, member (2) = deposit only, 3..7 = junior tiers (recruits, trials) with nothing until the owner defines them (`PUT {roles: [{rank, name, permissions}]}`). Every moderation call checks the caller's role bits instead of a fixed admin tier; `POST …/rank {ids, rank}` (needs PROMOTE) moves members to a rank junior to the caller's, never touching seniors; ranks 1..2 keep the legacy `state` in sync, so `promote`/`demote` still work |
| `GET /v1/groups/{id}/treasury`, `POST …/treasury/deposit`, `POST …/treasury/withdraw`, `GET …/treasury/ledger` | bearer | group treasury (5.4): a wallet per group and currency with its own ledger. `deposit {currency, amount, reason}` moves money from the caller's wallet (needs TREASURY_DEPOSIT, the member default; fails when the wallet is short), `withdraw {currency, amount, reason, to?}` pays the treasury out to a member (needs TREASURY_WITHDRAW; never below zero); both legs are one transaction and the member side shows in the personal ledger as `treasury_deposit`/`treasury_withdraw` with the group id as `ref`. Bus events `group.treasury_deposit` / `group.treasury_withdraw` |
| `GET /v1/groups/{id}/inventory`, `POST …/inventory/deposit`, `POST …/inventory/withdraw`, `GET …/inventory/ledger` | bearer | group inventory (5.4): a shared item store per group with its own ledger. `deposit {entry_id, quantity, reason}` moves stackable items out of the caller's own inventory entry (needs INVENTORY_DEPOSIT = 256, the member default; non-stackable items keep per-instance data and cannot be pooled), `withdraw {item_id, quantity, reason, to?}` grants them to a member (needs INVENTORY_WITHDRAW = 512; the store never goes negative, `max_stack` applies on the receiving side). Both legs run in one transaction; bus events `group.inventory_deposit/withdraw`. |
| `POST /v1/server/groups/{id}/xp`, `POST /v1/alliances`, `GET /v1/alliances/{id}`, `POST …/invite`, `POST …/accept`, `POST …/leave`, `GET /v1/groups/{id}/alliance` | server key / bearer | group progress and alliances (5.4): the server grants group XP (`{delta, reason}`), the level follows `[social] group_level_xp` thresholds (default 1 000 → level 2 … 45 000 → level 10), `level_up` lands in the group journal and `group.level_up` on the bus; groups carry `xp`/`level`. Alliances are groups of groups: a group owner founds one (`{group_id, name}`, the group leads), the leader's owner invites (`{group_id}`), the invited group's owner accepts or leaves (declining = leaving); a group is in at most one alliance; the leader leaving dissolves it. Journal: `alliance_founded/invited/joined/left/dissolved` |
| `PATCH /v1/friends/{id}`, `GET /v1/friends/recent`, `GET /v1/friends/suggestions` | bearer | friends depth: a private `note` (≤ 256 chars, `null` clears) and `favorite` flag per accepted friend (returned in the friends list); `recent` = up to 50 accounts you shared a match with, newest first (recorded when a session joins a relayed/authoritative match, both directions; blocked accounts hidden); `suggestions` = friends of friends ranked by mutual friends, then recent teammates — never anyone you already relate to or who blocked you. Accepted + outgoing friendships are capped at 1000 per account |
| `POST /v1/friends/import` {provider, subjects[], add?} | bearer | platform friends import: the client sends the platform ids of its Steam / Xbox / PSN / Nintendo / OIDC friends (≤ 500), the server maps them to accounts via linked identities and returns `matches` (subject, account, username); `add: true` also sends friend requests to every match (`friend.added` on the bus with `import: <provider>`). Unknown ids, the caller and disabled accounts are skipped |
| `POST/GET /v1/lobbies`, `POST /v1/lobbies/join`, `GET /v1/lobbies/mine`, `GET/PATCH/DELETE /v1/lobbies/{id}`, `POST …/{id}/leave`, `…/kick`, `…/ready`, `…/team`, `…/heartbeat`, `…/start` | bearer | lobbies (plan 5.4): waiting rooms of 2..=1000 accounts with optional team slots (`team_count` × `team_size` must cover `max_members`; joiners land on the least-loaded team unless they pick one), `public` (listed) or `private` (6-letter join `code`, shown to members only), optional password (argon2, `password: ""` in PATCH clears it), `region`, `metadata` ≤ 16 KB; listing = public + waiting, filters `name` prefix / `region` / `metadata` containment (`?metadata={"mode":"ctf"}`) / `has_slots` (default true), id cursor. One lobby per account; host leaving hands over to the longest-standing member, last one out closes it; host can kick, PATCH settings (never below current members/teams) and `start` once: `{fleet_id, payload}` allocates a dedicated server (roster goes in the allocation payload), `{module, params}` creates an authoritative match on the cluster, `{connect}` publishes the host's own connect info; `require_ready` refuses while someone is not ready. Every change reaches members as a notification `lobby` (code 20, `content.event` = joined/left/kicked/ready/team/updated/started/closed; started/kicked/closed are persisted, the rest are live-only). Lobbies without a heartbeat (any member) or change for 300 s are swept on the `lobbies` lease, members get `closed`. Lobby chat: a room channel named after the lobby id |
| `GET /v1/leaderboards[/{id}]`, `GET\|POST\|DELETE /v1/leaderboards/{id}/records`, `GET /v1/leaderboards/{id}/around/{owner}` | bearer | leaderboards: best/set/incr/decr operators, asc/desc, cron reset windows, 1-based ranks, owner lookup, around-owner paging |
| `POST /v1/server/leaderboards`, `DELETE /v1/server/leaderboards/{id}`, `POST\|DELETE /v1/server/leaderboards/{id}/records/{owner}` | `X-Musubi-Server-Key` | server-to-server: create/delete boards, authoritative writes with operator override `reset_schedule` is a 5-field cron in UTC, or crontab-style `CRON_TZ=Asia/Tokyo 0 0 * * *` for local midnight (DST-aware; tournaments too). |
| `GET /v1/tournaments[/{id}]`, `POST /v1/tournaments/{id}/join`, `GET\|POST /v1/tournaments/{id}/records`, `GET /v1/tournaments/{id}/around/{owner}`; server: `POST /v1/server/tournaments`, `DELETE …/{id}`, `POST …/{id}/records/{owner}` | bearer / server key | tournaments: start/end, active duration per reset window, max_size, max_num_score, join_required, category range listing |
| `POST /v1/server/tournaments {…, prizes: [{from_rank, to_rank, reward_table, title?, body?}]}`, `POST /v1/server/tournaments/{id}/end` | server key | tournament prizes (5.6): non-overlapping rank bands, each paying a reward table; when a window ends (leader sweep, or `end` by hand — which also fires `tournament_end` to modules once) every record in a band gets a mail with that table (`metadata {tournament_id, rank, score, expiry}`) and `tournament.prize` goes to the bus; paid once per (tournament, window) — `end` on a paid window returns `paid: 0`. `GET /v1/tournaments/{id}` shows `prizes`, listings do not |
| `POST /v1/server/tournaments {…, team: {perm?}}`, `POST /v1/tournaments/{id}/join {group_id}`, `POST /v1/tournaments/{id}/records {score, group_id}` | server key / bearer | team tournaments (5.6): records are owned by groups — a member holding the tournament's role bits (`perm`, default ANNOUNCE = 8) joins and writes for `group_id`; the tournament operator aggregates members' submissions (`best` keeps the best, `incr` sums), `max_num_score` caps the group, ranks / `around` work on groups. Team prizes are `{from_rank, to_rank, currency, amount}` and are paid into the group treasury (ledger entry without a member, `tournament.prize` on the bus) instead of by mail; `group_id` on a player tournament, or none on a team tournament, is a 400 |
| socket `matchmaker_add` / `matchmaker_remove` / `matchmaker_ready` → `matchmaker_ticket`, `matchmaker_ready_check`, `matchmaker_matched`, `matchmaker_ready_failed` | token | matchmaker: mutual queries (`+field:value`, `-field:value`, `>=`, `[a TO b]`, `*`), min/max count, count_multiple, 15 s wait for max before settling for min; named queues (`queue` property) with MMR and ping windows that widen with wait, fixed teams balanced by MMR, role quotas, cross-play groups, avoid list, no-repeat opponents, match quality grade, `matchmaker_score`/`matchmaker_propose` WASM overrides; ready-check (`matchmaker.ready_check_secs`, per queue) — confirmed tickets return to the queue with their wait, dodgers are kept out for `dodge_penalty_secs` doubling per repeat within 24 h (≤8×) |
| socket `party_create/join/leave/promote/accept/remove/close/join_request_list/data_send/matchmaker_add/matchmaker_remove` → `party`, `party_join_request`, `party_leader`, `party_presence_event`, `party_data`, `party_matchmaker_ticket`, `party_close` | token | parties: open or request-to-join, leader moderation, leader hand-off on leave, party-wide matchmaking tickets |
| socket `match_create`, `match_join` (by id or matchmaker token), `match_leave`, `match_data_send` → `match`, `match_data`, `match_presence_event` | token | relayed (client-hosted) matches, ≤64 players, targeted or broadcast data; authoritative matches come with the actor runtime |
| `GET /v1/matches?label=&min_size=&max_size=&open_slots=&region=` (free player slots need `params.max_players`; region is the hosting node's); server: `POST /v1/server/matches {module, params}`, `GET\|DELETE /v1/server/matches/{id}`; socket `match_join/leave/data_send` on an authoritative id | bearer / server key | authoritative matches as actors (`musubi-actor`): handler over an opaque state blob (init/join_attempt/join/leave/tick/terminate), tick 1–60 Hz, label ≤ 2 KB; built-in `echo` module; snapshots in `musubi.match_snapshots` on every membership change / every 50 ticks, claimed atomically and resumed on startup and whenever a node stops heartbeating (failover); match modules in WebAssembly: exports `match_<module>_{init,join_attempt,join,leave,tick,terminate}` (JSON in/out, state as base64) are registered automatically |
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

Cross-cell transactions: `POST /v1/server/worlds/{w}/tx {id?, ops: [{entity, components, expect?: {component: version}}], ttl_ticks?}` writes several entities — on any cells, any nodes — atomically: all of it lands or none of it does (trade, loot split, a door and its key across a border). When every entity lives on the receiving node the ops apply at once (`state: committed`). Otherwise the node holds its entities (writes to them answer 409, handover is deferred) and asks the other hosts to prepare; each checks its entities are present, not in transit, not held by another transaction and that every `expect`ed component version is still current, then holds them and votes. The last vote commits everywhere (`tx_committed` event to the owning simulators, persistent writes journaled), any refusal or a host silent past `ttl_ticks` (default 30) aborts everywhere (`tx_aborted` with the reason). `GET /v1/server/worlds/{w}/tx/{id}` → `pending | committed | aborted {reason}`; a known `id` on `POST` returns the stored outcome (`replayed: true`), so a retry never applies twice.

Server-side atomic transactions: `POST /v1/server/tx {ops: [...]}` (server key, up to 100 ops) runs storage writes (`{type: "storage_write", owner_id?, collection, key, value, version?}` — CAS and create-only `"*"` apply), `wallet_adjust` (balance checks, idempotency keys), `inventory_grant` and `inventory_consume` in one PostgreSQL transaction: a 409 on any op rolls back all of them, live storage subscribers hear about the writes only after commit. Grant limits are checked before and charged after the commit.

Cold cells: a hibernated cell (no client watched it for `hibernate_after_ticks`: lease revoked, ephemeral entities gone) that stays untouched for `unload_after_ticks` more (0 = never) is unloaded — its persistent entities are written to `musubi.world_cold_cells` and dropped from memory, so a huge persistent world costs RAM only where someone is. The cell is listed under `cold_cells` in `GET /v1/server/worlds/{w}`; interest, a lease or a spawn reaching it reads the row back (`cell_reloaded` event to the owning simulator) and a world moving to another node learns its cold cells from the same table.

Group handover (plan 4.5): an entity attached to a root (`World::attach` — passenger to vehicle, part to construction; chains allowed, cycles refused) never crosses a border on its own: when the root hands over, the whole group travels in one transaction — one `handover_request` carrying the root's state plus `group` states, one Ack flips authority and cells for all of them, inputs for any member are buffered meanwhile, and a cancel rolls every member back. Simulators declare groups with `POST /v1/server/worlds/{w}/attach {sim, entity, root}` / `…/detach {sim, entity}` (stream ops `attach`/`detach`; both entities must be owned by the caller; journaled, so a rehome keeps the group). A handover that gets no Ack within `handover_timeout_ticks` (3) is cancelled and backs off before the entity is offered again: 2, 4, 8 … up to 32 ticks per consecutive failure (`handover_retry_tick`), reset on the next successful Ack — so a slow or flapping neighbour is retried, not hammered every tick.

Interest v2: a client's `world_subscribe` carries an optional `budget_bytes` per tick (default `client_budget_bytes` = 16 KB in the world config) and an optional `filter` (component → value every delivered entity must match) on top of the radius. Each tick the entities in range are ranked by `relevance × (1 + age)` — relevance falls with distance, age is the ticks since the client last heard about the entity — and sent in that order until the estimated bytes exceed the budget (at least one always goes); the rest keep their old state and come back later with a higher priority, so a crowded radius degrades to lower update rates for far entities instead of bursts. Despawns are always delivered. Positions are quantized to `pos_quantum` (0.01) and jitter below it is not an update.

Degradation under load (plan 4.8): `soft_cap` in the world config refuses spawns into a cell that already holds that many entities (`429`, event `cell_full` to the simulator; handovers always pass — physics cannot wait at a border). A spawn with `queue: true` (HTTP and stream op) waits instead: `202 {queued, ticket, cell, position, eta_ticks}` and event `queued`; each tick the cell admits waiting spawns in arrival order as entities leave (`queue_admitted` with the entity), reports position/ETA changes (`queue_update`, ETA = position / smoothed departures per tick, `null` until the cell has seen departures) and drops tickets older than `queue_ttl_ticks` (600) or whose simulator lost the cell (`queue_expired` with `timeout` / `lease_lost`). Cell cost (plan 4.4 dynamics): `cost_entity`·entities + `cost_player`·clients whose interest is centred in the cell + `cost_traffic`·entities in handover out of it (smoothed per tick), plus `cost_lag` per tick the owning simulator lags; a simulator over `split_cost` sheds its least-coupled cell to one that stays under the bar, and one under `merge_cost` for `merge_after_rounds` control rounds (30 ≈ 30 s) hands every cell to the owners of neighbouring cells that stay under ¾ of `split_cost` — all or nothing — and is left free (co-locate: ghosts and handovers stop crossing simulators; the hysteresis keeps it from flapping). Split before dilation: a simulator whose reported tick lags the world by more than `tidi_after_ticks` first sheds a cell (the one least coupled to its others) to an alive simulator that keeps up, regardless of `split_cost` (reconcile reason `lagging`); only what it still holds is dilated. Time dilation: a simulator reports the last tick it fully processed (`tick` in `POST /renew`, op `tick_done` in the stream); when it lags the world by more than `tidi_after_ticks` (15) its cells run at `tidi_after_ticks / lag` of real time, not below `tidi_min` (0.1) — the simulator gets a `time_scale` event and sees `time_scale` per cell in its tick frames, clients get `time_scale` in `world_delta` (slowest cell in their radius) to interpolate against; the cell returns to 1.0 as the simulator catches up. Interest shedding is the per-client budget of interest v2.

World persistence (plan 4.9): a spawn may carry `decay_ticks` — the entity disappears after that many ticks without a write, interaction or handover (event `decayed` to the cell's simulator; persistent ones are journaled so no shard resurrects them) — abandoned litter cleans itself up. A cell no client has watched for `hibernate_after_ticks` (1800 ≈ 60 s; 0 = never), leased or holding entities under a lapsed lease, with nothing in transit or written to recently, hibernates: its lease is revoked (`cell_hibernated` with `ephemeral_despawned` / `persistent_kept` to the last simulator), ephemeral entities die with it, persistent ones stay in the shard's memory and snapshot, and the cell is not an orphan — nothing re-leases it while nobody looks. A client's interest reaching it (or a simulator leasing it) wakes it within one control round with its persistent entities in place. Old snapshots and journals load unchanged (the new entity fields default). Time machine: every shard snapshot is also appended to `musubi.world_history` (retention `world_history_days`, 7) and the journal is kept back to the oldest retained history row, so `GET /v1/admin/worlds/{w}/at?tick=T` rebuilds the persistent entities as of any retained tick — newest history snapshot of each shard at or before T plus the journal ops up to T (provenance-aware merge, as in recovery) — and reports the retained range; `POST /v1/admin/worlds/{w}/snapshot` takes one now (shards that left the world contribute their last snapshot only).

Lag compensation: the shard keeps `history_ticks` (15 ≈ 500 ms) of positions per entity. `POST /v1/server/worlds/{w}/rewind {entity, tick, x?, y?, tolerance?}` (and the stream op `rewind`) answers where the entity was at the end of `tick` and whether `(x, y)` was within `tolerance` (1.0) of it — `pos`/`hit` are null when history does not reach that far back, never a guess. An `interact` carrying the actor's `tick` (and `x`, `y`, `tolerance`) reaches the target's owner with the same answer under `rewind`, computed on the owner from its own history whether the interaction came from a local simulator or over the bus — Source-style rewind for hitscan on a ghost across a border. Co-locate (docking, grappling — physics that cannot be split across two owners): `POST /v1/server/worlds/{w}/colocate {sim, entity, with, ttl_ticks}` (stream op `colocate`) pins `with` into `entity`'s cell for `ttl_ticks` (300) — the regular handover brings it over with state and buffered inputs (its owner hears `co_locate`, then `handover_request` reaches the caller, who acks), the caller is authoritative over both while the pin holds, and when it lapses the partner hands back to wherever its position says. A partner living on another node is reached over the bus: the caller pins first (so the arrival lands in its cell), the node hosting the partner pins too and its regular cross-node handover ships the entity; the answer carries `forwarded: true`.

Load: `musubi bench --scenario world --clients N --worlds W --entities E --radius R` creates W arenas with E entities each moved by a simulator every tick, spreads N WebSocket players over them and reports frames per player per second, inter-frame gap p50/p99, bytes and entity updates per second, plus the server's own `musubi_world_tick_duration_seconds` (simulation step) and `musubi_world_fanout_duration_seconds` (per-player delta build and send) from `/metrics`. Players walk their interest circle in short random hops (5 Hz), as a moving player would; arenas are deleted when the run ends. Every world ticks on its own thread with its own clock (a slow world never holds back the others); a player whose view did not move re-examines only the entities that changed since its last delta (per-cell change ticks plus a change log), deltas are built sequentially on the world's own thread, component values travel as JSON text serialized once per change, and `--max-hz N` caps deltas per player per second (skipped ticks fold into the next delta). Reference on an 8-core dev box, `--wire binary`: 1 000 players over 4 worlds × 500 entities → 30.1 frames/player/s (the full 30 Hz), gap p50 33 ms / p99 47 ms, 0 errors, ≈0.8 cores. 10 000 players over 20 worlds × 500 entities on the same box: every player connects (the handshake no longer reads the account row — the username rides in the token — and the listen backlog is 4096) and the box is the limit: in the steady window the server takes 6.3 of the 8 cores (half of it the twenty tick threads, half the runtime workers) and the load generator the rest, the simulator (clock-paced, see `sim_ticks_per_sec`) reaches 21.2 Hz and players get 21.4 frames/s each, 0 errors, 1.99 M entity updates/s at 138 MB/s, `apply` ≈35 ms (runtime starved, not the world lock). Found and fixed on the way: glibc malloc's arena locks (most runtime-worker samples sat in realloc/free waiting for one; the server binary uses mimalloc), the world mutex taken by every subscription and `home_of` on a session task (interest is now queued and applied by the world's tick thread, the home lookup reads a mirror), the tick holding the lock while building deltas (deltas are built from a `WorldView` snapshot off the lock), deltas that cloned every component's JSON value per client and re-serialized it to estimate its size (they carry shared text now), and a benchmark that paced its simulator by the server's reply and so under-reported frames. What remains is CPU per frame — about 30 µs across build, encode and write — so 30 Hz for 10 000 players on one node of this size is still open; the next lever is sharing the serialized component text between clients instead of cloning it per frame.

Fencing of world persistence (plan 4.4): a node writes `world_snapshots` / `world_history` / `world_journal` rows only while it holds a current term in `musubi.world_hosts` (`insert … where exists`), taken when it creates the world, is assigned a cell through the directory or claims a dead home in `recover`. The node that takes a world or cell over deletes the loser's row, so a host cut off from the quorum but still reaching Postgres gets `fenced` on its next persist instead of overwriting the state the new host runs on; its last pre-fence snapshot remains what the new host restored from.

Multi-region placement: `POST /v1/server/worlds` takes `region`; a world for another region with a live node there is created as an unowned row (`202 {pending: true}`) that a node of that region adopts on its next failover round (≤ 0.5 s), and failover itself prefers nodes of the world's region — a node elsewhere adopts only when none of them is alive (`musubi.worlds.region`, migration 0049). Without a live node in the asked region the world is simply placed on the creating node.

Operators: `GET /v1/admin/worlds/{w}/entities` dumps this node's shard of a world — every entity with cell, position, transit state and the live leases (where is entity X right now?); `musubi soak --rejoin-delay-secs N` keeps a killed node down for N seconds so the survivors alone must account for every entity.

Shard rows are merged with provenance: when a world is rebuilt from several nodes' snapshots and journals (home hand-off at drain, a host that died, a host that handed its cell back), a `Despawn`/`Apply` from node X only touches entities whose current copy came from X — a stale "it left my shard" record of a former host can no longer erase the fresh home snapshot. Hand-offs also stop a node from persisting stale rows afterwards: a drained home drops its worlds right after the hand-off snapshot, a shard that gave up its last cell (drain, reassignment, boot hand-back after a restart) is dropped with its journal buffer, and cell assignments consulted during adoption and pruning are confirmed through the Raft leader. Found by the soak with `--victim leader` and a 1 s rejoin: 1 in 2–3 runs lost 5–15 persistent entities at drain; three consecutive runs since are at 0.

## Integrations (Zanshin LiveOps, Aurix voice)

`[zanshin] url, server_key, flush_secs` and `[aurix] url, api_key, party_channel_type, match_channel_type` in the config (unset `url` = disabled). The Musubi account id is the `external_id` on both sides.

- Zanshin: authentication is tracked as `musubi.authenticated` (batched `POST /server/v1/events`); clients evaluate flags / live events for themselves via `GET /v1/liveops/flags?names=` and `GET /v1/liveops/live-events` (proxied with the server key, never exposed).
- Aurix: `POST /v1/voice/token {party_id | match_id, display_name?, speak?}` checks membership on this node and mints a player token for the ad-hoc channel `party-<id>` / `match-<id>` (created on first join, removed when empty); the response carries Aurix's `token`, `user_id`, `expires_at`, `channel_id`, `endpoint`.

## Economy v0 (`musubi-economy`)

Currencies with optional caps; wallets with an append-only ledger (`delta`, `balance_after`, `reason`, `ref`, idempotency key — a replayed key returns the original entry, balances never go negative); item catalog (stackable with `max_stack` or unique instances with `instance` JSON); store listings priced in several currencies with `max_per_account`, purchase = debit + grant in one transaction, idempotent. Players: `GET /v1/wallet`, `GET /v1/wallet/ledger?currency=&limit=`, `GET /v1/inventory`, `POST /v1/inventory/consume {id, quantity}`, `GET /v1/store`, `POST /v1/store/buy {store_item_id, currency, idempotency_key?}`. Server key: `GET|POST /v1/server/currencies`, `POST /v1/server/wallet/adjust {account_id, currency, delta, reason, ref, idempotency_key, metadata}`, `GET|POST /v1/server/items`, `POST /v1/server/inventory/grant`, `GET|POST /v1/server/store`. Rewards tables, progression, battle pass, auctions and IAP validation are next.

**Inventory rules.** An item definition carries `unique` (one entry per account; a second grant is `409`), `bind` (`none` | `pickup` — bound on grant | `equip` — bound on first equip), `tradeable` (false = never leaves the account) and `slots` (equipment slots it fits). Players `POST /v1/inventory/equip {id, slot}` / `unequip {id}` — a slot holds one entry, equipping into an occupied slot takes the old entry off; entries report `bound` and `slot`. Bound, equipped or untradeable entries are refused by the market, direct trades, the order book and group deposits. Servers patch instance data with `POST /v1/server/inventory/{id}/instance {account_id, merge}` (shallow merge, `null` deletes, validated against the item's `instance_schema` — durability, level, enchants). `PUT /v1/server/inventory/limits {category, max_entries}` caps how many entries (stacks/instances) of a category an account can hold (`429` past it; `0` removes the cap).

### Content bundles (environments)

Definition tables — currencies, items, reward tables, progression tracks, quests, store items, coupons, rating queues — travel between environments as one document. `GET /v1/admin/content` exports them (`{format: 1, tables: {currencies: [...], ...}}`); `POST /v1/admin/content/import {bundle, dry_run?, prune?, note?}` applies a bundle in one transaction: rows are upserted parents-first (a row may carry only the columns it sets), `prune: true` also deletes rows the bundle does not list (children first), a foreign-key or check violation rolls everything back with 409 and the offending row. The reply carries a per-table diff (`added`/`changed`/`removed` counts and rows) — run with `dry_run: true` to review a dev → stage → prod promotion before it lands. Every applied import stores the state before it and the state after it in `musubi.content_versions` (`GET /v1/admin/content/versions`, `GET …/versions/{id}`); `POST …/versions/{id}/rollback {note?, prune?, dry_run?}` re-applies that version.

**Deferred rewards.** A reward table with `deferred: {delay_secs, ttl_secs?, title, body}` does not pay out on roll/grant: the outcome (odds, pity and modifiers already applied) is frozen into a mailbox message with `reward_entries` and `available_at`; the roll/grant reports the `deferred` / `mail_id`, and the player claims it with `POST /v1/mail/{id}/claim` once it is available (`403` before, expires after the TTL). `POST /v1/server/mail` also takes inline `reward_entries` plus `available_in_seconds` for compensations and scheduled gifts without a table.

### Operator roles

Keys issued through `POST /v1/admin/keys {name, role}` carry a role: `server` (game servers — everything outside `/v1/admin/`), `admin` (everything), and three console roles that read everything but write only their own area — `support` (accounts, bans, kicks, reports, login queue, chat moderation, mail), `liveops` (content bundles, currencies, items, rewards, progression, quests, seasons, store, entitlements, grant limits, rating queues, leaderboards, tournaments), `ops` (fleets, builds, worlds, cluster, webhooks, integrations). A call outside the role's scope answers 403 naming the key and the route; only `admin` keys issue or revoke keys. The admin audit log records the key name on every mutation, so a console with per-operator keys gets per-operator history for free.

### Subscriptions

Platform friends: `POST /v1/friends/import {provider, subjects, add?}` maps the console/launcher friend list to accounts and remembers the ids, so when one of those friends links that identity later the match is made by the server — `friend.platform_match` event, and the friend request is sent when the import had `add: true` (`social.platform_match_secs`, one node by lease). `GET /v1/friends/import?provider=steam` shows the stored ids and who they resolved to; `POST /v1/server/friends/platform-match` runs the matcher on demand.

Achievements carry `platform_ids` (`{"steam": "ACH_WIN", "psn": "trophy_7", "xbox": "..."}`): on unlock the `achievement.unlocked` event lists them for the client SDK (PSN trophies / Xbox achievements can only be unlocked on the device), and Steam is unlocked server-to-server through `ISteamUserStats/SetUserStatsForGame` with the publisher key from `[iap.steam]` for the player's linked Steam identity (`achievement.sync_failed` on error).

Mail broadcasts take an `audience` instead of `account_ids`/`all`: `{"audience": {"lang": ["de"], "location": ["eu"], "platform": ["psn"], "age_bracket": ["adult"], "track": "xp", "min_level": 10, "active_within_days": 7 | "inactive_days": 30, "created_after": <unix>, "metadata": {"cohort": "b"}}}` — every listed condition must hold; `POST /v1/server/mail/audience/count` previews the reach before sending.

Purchase-velocity heuristics: `[fraud] enabled = true`, `window_secs`, `max_purchases`, `max_spend = { gold = 100000 }`, `max_iap`, `action = "flag" | "block"` — an account over a limit inside the window gets a fraud flag (`economy.fraud_flag` event, `GET /v1/server/economy/fraud/flags`, `POST …/flags/{id}/resolve`, `GET /v1/server/economy/fraud/{account}` for live counters) and with `block` the purchase or IAP redeem is rejected with 429.

Console and launcher stores verify server-to-server: `POST /v1/iap/{psn|xbox|nintendo|epic}` `{product_id, token?}` asks the platform what the player's linked identity owns (PSN Entitlement Web API, Microsoft Store Collections — `token` is the user collections id from the client —, Nintendo consumable service items, Epic Ecom), consumes one use of a consumable, and grants the mapped catalog product through the same receipt path as Apple/Google (idempotent per entitlement / consumption id). `[iap.psn] / [iap.xbox] / [iap.nintendo] / [iap.epic]`: `client_id`, `client_secret` (+ `tenant_id` for Xbox, `sandbox_id`/`deployment_id` for Epic, `url` for Nintendo); endpoints and the `verify_path`/`consume_path` templates default per platform and are overridable.

Huawei AppGallery and Samsung Galaxy Store work like Google Play — the client sends what the store SDK returned: `POST /v1/iap/huawei {product_id, purchase_token}` (`[iap.huawei] app_id/app_secret`, Order Service `url`/`sub_url` for your region; consumables are confirmed server-to-server after the grant so a token cannot be redeemed twice) and `POST /v1/iap/samsung {product_id, purchase_id}` (`[iap.samsung] service_account_id/private_key_pem_path` of a Galaxy Store Developer API service account; receipt via `/iap/v6/receipt`, optional `acknowledge_path`). Both map store products through the same `/v1/server/iap/products` catalog and are idempotent per order.

An IAP product mapped with `kind: "subscription"` (plus `grace_secs`) is auto-renewing: redeeming its receipt (`POST /v1/iap/apple` with the transaction id, `POST /v1/iap/google` with the purchase token — looked up through `purchases.subscriptionsv2`) grants the store item for the period and keeps one `iap_subscriptions` row per original transaction (Apple `originalTransactionId`, Google purchase token); every renewal is a new transaction, so the item is granted again and the period end moves forward. `GET /v1/iap/subscriptions` (and `GET /v1/server/iap/subscriptions?account_id=`) report `status` as of now — `active`, `grace` (period ended, still inside `grace_secs`), `expired`, `revoked`, `hold` — with `active: true` while the player keeps access. App Store Server Notifications V2 (`SUBSCRIBED`, `DID_RENEW`, `DID_FAIL_TO_RENEW`, `EXPIRED`, `GRACE_PERIOD_EXPIRED`, `REFUND`, `REVOKE`, renewal preference from `signedRenewalInfo`) and Google RTDN `subscriptionNotification` (types 1–13) update the row and emit `iap.subscription.<type>` events.

### Web Push and e-mail

`[push.webpush] vapid_private_key_pem` (PKCS#8 P-256) turns on Web Push next to FCM/APNs: browsers subscribe with the `vapid_public_key` returned by `GET /v1/account/push_tokens` and register the `PushSubscription` JSON as a `webpush` token; a notification that reaches no live socket is encrypted per RFC 8291 (aes128gcm, fresh ECDH key and salt per message) and posted with a VAPID `Authorization` header; 404/410 from the push service deletes the subscription.

`[email] host/port/tls/username/password/from` enables the built-in SMTP mailer (STARTTLS, implicit TLS or plain; AUTH PLAIN) for `account.email_verification` and `account.password_reset`: the mail carries `verify_url` / `reset_url` with `{token}` substituted (or the bare token when no page is configured) and the outcome is recorded as `email.sent` / `email.failed`. The original events still reach webhooks and modules, so a game may keep its own mailer instead.

### Collection contracts, migrations, indexes and live objects

`PUT /v1/admin/storage_schemas/{collection}` (liveops/admin) attaches a contract to a storage collection: `schema` is a JSON Schema subset (`type`, `properties`, `required`, `additionalProperties`, `items`, `enum`/`const`, numeric and length bounds, `pattern`, `anyOf`/`oneOf`/`allOf`/`not`, `$ref` into `$defs`) enforced on every write and patch (400 names the offending path); unknown keywords are rejected up front. `version` + `migrations` (`[{from, to, ops: [{op: set|default|rename|remove, …}]}]`) move objects written under an older version forward when they are read or patched, and `POST …/migrate?batch=` rewrites them in place until `remaining` is 0. `indexes: [{field, kind: text|number, unique?}]` become partial expression indexes on the collection; `GET /v1/storage/{collection}?where=` accepts `"$where": [[field, op, value], …]` (`= != < <= > >=`, numbers compared numerically) next to the containment filter, on the same indexed expressions. Over the realtime socket `storage_subscribe {collection, key?, owner_id?}` streams `storage_update {key, version, value | deleted}` for one object (another user's object must be readable) or the caller's whole collection, across nodes via the fanout bus. From the CLI: `musubi schema push profiles.toml` (a file with `collection`, `version`, `schema`, `migrations`, `indexes`), `musubi schema list`, or `[[schemas]]` entries in `musubi.deploy.toml` — `musubi deploy` applies them before publishing modules, so contracts land before the code that relies on them.

### Tournament brackets

`POST /v1/server/tournaments/{id}/bracket {format: single|double|swiss|round_robin, seeding?: score|join|random|list, participants?, size?, swiss_rounds?, deadline_secs?}` builds one bracket per tournament from its current participants (standings order by default): standard seeding (1 v n, 2 v n-1 …), byes for non-power-of-two fields resolved immediately, a losers bracket plus grand final for double elimination (no bracket reset), and Swiss rounds paired by wins/Buchholz without rematches (an odd player out gets a bye; `POST …/bracket/rounds` pairs the next round once the current one is decided). Matches are decided by `POST …/bracket/matches/{mid} {winner, score?}` or by the end-game report with `bracket_match: {tournament_id, match_id}` — the rank-1 team's participant wins; the same winner twice is a no-op, a different one is 409. `GET /v1/tournaments/{id}/bracket` returns the matches with `status` (pending/ready/done/void), `ready` ids, standings (wins, losses, byes, Buchholz, eliminated) and the champion; `tournament.bracket_created/match/round/done` go to the event bus. Round-robin builds every round up front (circle method, one bye a round for an odd field) and crowns the top standing. With `deadline_secs`, each ready match carries a `deadline`; past it `POST …/bracket/matches/{mid}/forfeit {participant}` hands the absent side a loss (`score: {forfeit}`).

### Crafting

`PUT /v1/server/crafting/recipes {id, inputs: [{item_id|currency, amount}], outputs: [{item_id|currency|reward_table, amount, instance?}], requires_item?, max_batch?, max_per_account?, enabled}` defines a recipe (referenced items, currencies and tables must exist). `POST /v1/crafting/craft {recipe_id, count?, idempotency_key?}` consumes the inputs (largest stacks first) and grants the outputs in one transaction — short inputs answer 429 with `have N, need M` and nothing moves; a required item (blueprint) must be owned but is kept; `max_per_account` is a lifetime cap and limited grants on output items/currencies are checked before anything is consumed. A repeated `idempotency_key` replays the stored result. `GET /v1/crafting/recipes` lists enabled recipes, `GET /v1/crafting/log` the caller's crafts; `economy.crafted` goes to the event bus.

### User-generated content

`POST /v1/ugc {kind: map|mod|skin|blueprint|replay|level|other, title, description?, tags?, url?, visibility?: public|unlisted|private}` registers a draft; `PUT /v1/ugc/{id}/blob` (raw body, ≤ 16 MiB, `Content-Type` kept) stores the content with its sha256 and bumps `version`; `POST …/submit` hands it to moderation. Moderators (`support`/`admin`) see `GET /v1/admin/ugc?status=pending` and decide with `POST /v1/admin/ugc/{id} {status: approved|rejected|hidden|pending, reason?}`; only approved items are browsable (`GET /v1/ugc?kind&tag&q&sort=new|top|downloads&owner_id`) and downloadable (`GET …/blob`, counts downloads, `ETag` = sha256, `x-ugc-version`). A new blob on an approved item goes back to `pending`, so subscribers (`POST …/subscribe`, `GET /v1/ugc/subscriptions` with current versions) only ever get moderated revisions. Ratings are 1–5 stars, one per account (`POST …/rate`), and everything emits `ugc.*` events.

### Webhook fleet provider

`[fleet.webhook] url = "https://orchestrator.example/musubi"` registers `provider = "webhook"`:
Musubi starts a server with `POST {url}/servers` (`{server_id, port, fleet{id,name,region,pool,image,command,vcpu}, env}`,
env already holds `MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY`), expects `{id?, address, port}`
— or `202` + `id` and polls `GET {url}/servers/{id}` until `{status: "ready", address, port}` —
and stops it with `DELETE {url}/servers/{id}`. Three endpoints put GameFabric, a home-grown
scheduler or a bare-metal panel behind the same fleets, buffers, autoscaling and allocation
API as Docker, Kubernetes, Firecracker, Edgegap and GameLift.

### Firecracker snapshots and warm pool

A microVM that boots a kernel takes a second; one restored from a snapshot takes tens of
milliseconds. With `[fleet.firecracker] snapshot_dir = "/var/lib/musubi/firecracker/snap"`,
`POST /v1/server/fleets/providers/firecracker/prepare` boots one VM with `musubi.warm=1`, waits
up to `warm_boot_secs` for the guest to print `musubi: warm` (server loaded, nothing bound),
pauses it and writes a full snapshot (`vmstate` + `mem`). From then on every start spawns a bare
VMM, `PUT /snapshot/load`s the snapshot paused, hands the identity over MMDS (the kernel command
line is frozen in the snapshot) and resumes. `warm_pool = N` keeps N VMs restored-and-paused
ahead of demand — a start then is MMDS + resume — and a background task refills the pool.
Guest contract on resume: `GET http://169.254.169.254/` returns `{server_id, fleet_id, port,
api, server_key, env, ip?, gateway?, mask?}`; configure `eth0` from `ip/gateway/mask` when present
(each VM gets its own tap and address via `network_overrides`) and start the server exactly as
after a cold boot. Re-run `prepare` after a build change — the snapshot carries the loaded binary.

### Durable workflows (sagas)

Long-running server logic — a trade with a 24-hour escrow, a tournament payout, a cross-game
grant with rollback — is a module export `workflow_<name>` called one step at a time with its
persisted state. `POST /v1/server/workflows {name, input, key?, delay_secs?, account_id?,
max_attempts?}` starts one (`key` makes the start idempotent); each step receives `{id, name,
step, attempt, input, state, signal, timed_out}` and answers `{state?, sleep_secs | wait_signal
(+ timeout_secs) | done (+ output) | fail}`. A trap retries with exponential backoff up to
`max_attempts`; a permanent failure or `POST …/{id}/cancel` runs `workflow_<name>_compensate`
when the module exports it (the saga's undo) and the workflow ends `compensated`.
`POST …/{id}/signal {payload}` wakes a parked workflow with that payload; `GET …/{id}` shows
state, step history (last 50) and the next wake. One node runs due steps cluster-wide (lease
`workflows`), rows are claimed with `skip locked`, and the state is the row — a restart loses
nothing. `musubi.workflows` is also plain SQL for your dashboards.

### Age gate and parental controls

`PUT /v1/account/age {birthdate | bracket}` puts an account in an age bracket — `child` (under
`compliance.child_until`, 13), `teen` (under `adult_from`, 18), `adult` — from a birthdate the
player gives or a platform's age signal; `PUT /v1/server/accounts/{id}/age` does it from a
trusted source and a child cannot raise its own bracket. Per bracket `[compliance]` lists the
closed features — defaults: children lose `chat` (sending; reading stays), `friends` (requests),
`purchases` (store + IAP), `ugc` and `public_profile` (lookups show only id/username/avatar);
`unknown_as_child = true` treats accounts without an age as children. `GET /v1/account/age`
tells the client what is closed. `POST /v1/account/parental/request {parent_email}` emits
`account.parental_consent_request` with a single-use token for your mailer;
`POST /v1/auth/parental/consent {token}` (no session) records the parent's consent, after which
only `consented_child_restrictions` (`purchases` by default) remain. The view lives in memory on
every node like bans, so a change bites on the next request.

### Storage: JSON Patch, aggregates, full-text

`POST /v1/storage/patch` takes either a merge patch (`patch`, RFC 7386) or RFC 6902 operations
(`ops`: add / remove / replace / move / copy / test — a failing `test` rejects the batch with 409,
so read-modify-write races become guarded patches). `GET /v1/storage/{collection}/aggregate?field=stats.score&group=mode`
rolls a numeric field up (count / sum / avg / min / max, optionally per group) under the same
`where` / `prefix` / `owner_id` filters as listing. `"$q": "flaming sword"` inside `where`
is a websearch-style full-text match over the whole value (GIN-indexed), combinable with
containment and `$where` on `list` and `count`.

### Magic links, custom auth, session cap

`POST /v1/auth/email/magic/request {email, create?}` emits `account.magic_link` with a single-use
token (15 min) for your mailer; `POST /v1/auth/email/magic {token, vars?}` turns it into a session
and marks the address verified — passwordless login without account enumeration. A trusted game
server mints sessions for identities it verified itself with `POST /v1/server/auth/custom {id}`
(server key). `session.max_sessions_per_account` caps live sessions per account; a new login
evicts the oldest.

### Progress graphs: skill trees and timed unlockables

`POST /v1/server/graphs` defines a graph of nodes; each node has prerequisites (`requires`), a
price per rank (`cost: {currency: amount}`), an optional level gate (`min_level: {track, level}`),
`max_rank`, a reward table granted per rank and an opaque `grants` payload for the game. A node
with `unlock_seconds` is a timed unlockable: `POST /v1/graphs/{g}/nodes/{n}/unlock` pays and
starts the timer, `…/claim` finishes it once it ran out, `…/skip` pays `skip_cost_per_hour` ×
remaining time (rounded up) to finish now. `GET /v1/graphs` returns every node with the player's
`rank`, `state` (`locked` / `available` / `unlocking` / `ready` / `maxed`), `blocked_by`,
`unlocks_at` and the current `skip_cost`; `POST /v1/graphs/{g}/reset` pays `reset_cost`, refunds
`refund_pct` of what was spent and locks everything again.

### Reward modifiers

`PUT /v1/server/rewards/modifiers {id, scope, factor, starts_at?, ends_at?, account_id?}` multiplies
rewards for a time window — for everyone or for one player. Scopes: `xp` / `xp:<track>` (XP added
to a track), `currency` / `currency:<code>` (currency entries of any reward table, quest, level,
achievement, mail…), `table:<id>` (one reward table). Active modifiers multiply together; items are
never multiplied. `POST /v1/server/progression/xp` reports `xp_factor` and `delta_applied`, players
see what is active in `GET /v1/rewards/modifiers`.

### Quests: rerolls

A quest with `rerollable: true` can be swapped out by the player (`POST /v1/quests/{id}/reroll`):
it disappears for the rest of its period, `rerolls_per_period` caps how many quests of that
kind a player may reroll per period, completed quests stay. `GET /v1/quests` carries
`rerollable` and `rerolls_left` per quest.

### Store: per-platform listings

A store listing carries `platforms` (`steam`, `ios`, `android`, `psn`, `xbox`, `nintendo`,
`epic`, `web` …; empty = everywhere). Clients call `GET /v1/store?platform=ios` and only see
what is open to them — the same catalogue item can be priced or bundled differently per store.

### Presence: rich status and appear offline

`status_update {status?, rich?, appear_offline?}` — `rich` is a JSON object (≤ 2 KB: activity,
party size, joinable …) followers receive next to the status text; `appear_offline: true` sends
followers a leave and keeps the session out of `status_follow` snapshots until it is turned off
(a lone join), while the server still treats the player as online for delivery.

### Notifications: TTL, push templates and schedule

`POST /v1/server/notifications` takes `ttl_secs`: the notification carries `expire_time`, leaves
the inbox and the unread count when it passes, and is purged by retention. Push grew a server
surface: `PUT /v1/server/push/templates` (title/body/data with `{{var}}` placeholders;
`{{username}}` is always set), `POST /v1/server/push {template_id | title/body/data, vars,
account_ids | everyone with a token, send_at | local_date + local_time}` — immediate, at a UTC
instant, or at a wall-clock time in each recipient's own timezone (`accounts.timezone`, rolled
out zone by zone by the `push_schedule` lease holder every 30 s) — and
`GET/DELETE /v1/server/push/schedule`.

Platforms: `fcm`, `apns`, `webpush` and `hms` (Huawei Push Kit: `[push.hms] app_id/app_secret`,
app-level OAuth token cached until it expires). `data.silent = true` sends a background push —
FCM data-only message, APNs `content-available: 1` with `apns-push-type: background`, HMS data
message — so the app wakes and reads `data` without showing a banner. `data.link` (or `data.deep_link`) is where a tap lands — an `https://` page or an app scheme like `mygame://shop/offer/7`: FCM `fcm_options.link` / Android `click_action`, APNs top-level `link` (+ `url-args`), HMS `click_action` URL/intent, Web Push `url`; templates can build it (`"link": "mygame://quest/{{id}}"`).

### Chat: typing, attachments, automod

`channel_typing {channel_id}` fans a `channel_typing {channel_id, user}` event to everyone else
in the channel (at most one per 2 s per channel, never stored). `content.attachments` is a
checked contract — up to 10 `{url, name?, mime?, size?}` with `https://` or `ugc:<id>` URLs and
sizes ≤ 25 MiB — so clients can render it without defensive parsing. `chat.automod_url` points
at your moderation service: every message is POSTed as `{channel_id, sender_id, username,
content}` and the answer `{"action": "allow" | "block" | "replace", "content"?, "reason"?}` is
applied after the built-in filters and the `before_chat_message` hook; `automod_timeout_ms` and
`automod_fail_open` decide what a slow or dead service means.

`chat.translate_url` adds translation: a message with a `text` is POSTed as `{channel_id,
sender_id, text, source, targets}` — `source` is the sender's account language, `targets` the
other languages of everyone present in the channel (at most `translate_max_langs`, nothing is
sent when all speak one language) — and the answer `{"translations": {"de": "…"}}` rides on the
message as `content.translations` (+ `source_lang`), stored with it so history has it too; each
client renders its own language. Errors and timeouts (`translate_timeout_ms`, 800 ms) never hold
a message back. Plug in DeepL, Google, Azure or an LLM behind that one endpoint.

### Matchmaker observability

`GET /v1/server/matchmaker/stats` adds `wait` — p50/p95/p99/max wait per queue × region over the
last 2 048 matched tickets — and `waiting_ages` (how long the tickets in each queue have been
there), next to the Prometheus histogram `musubi_matchmaker_wait_seconds`.
`POST /v1/server/matchmaker/simulate {queue, arrivals_per_sec, duration_secs, mmr_mean, mmr_sd,
regions, party_sizes, seed}` dry-runs a queue's rules on virtual time with synthetic players and
returns matches, wait percentiles and quality (MMR gap, ping) — tune `mmr`/`ping` expansion before
real players feel it; same seed, same answer.

### Player statistics

`PUT /v1/server/stats/defs` declares typed counters (`sum` / `max` / `min` / `last`, each
`public`, `friends` or `private`); `POST /v1/server/stats/update {account_id, scope?, stats}` folds
values into a scope slice (mode, season, map …) and the global slice at once;
`GET /v1/server/stats/{account_id}`, `GET /v1/server/stats/top?stat&scope&limit`. Players read
their own with `GET /v1/stats[?scope]` and others' with `GET /v1/stats/{account_id}` (public, plus
`friends` stats for friends). `POST /v1/server/matches/{id}/results` with `stats_scope` folds each
player's `stats` object in the same report that settles rating, rewards, quests and XP.

### Anti-cheat signals

Every match input in an authoritative match passes a per-session guard (`[anticheat] max_input_bytes`, `max_inputs_per_sec`): an oversized or too-frequent input is rejected and recorded as a signal. Game servers add their own with `POST /v1/server/anticheat/flag {account_id, kind, severity: 1..=100, details?, match_id?}`, modules with the `anticheat.flag` host op. Signals sum into a per-account score over `window_hours`; when it reaches `kick_score` the account is kicked from every node (`anticheat.kicked`). Moderators read `GET /v1/admin/anticheat?min_score` (worst accounts first) and `GET /v1/admin/accounts/{id}/anticheat`. `POST /v1/server/anticheat/verify {provider: eac|battleye, account_id, ticket}` relays a client session ticket to the configured vendor endpoint (`eac_url` / `battleye_url`, bearer key) and treats a rejection as a `vendor_rejected` signal. Everything emits `anticheat.signal`.

### Georouting and fleet pools

`GET /v1/regions` lists the regions a client should probe — `[regions] regions = [{name, echo, http}]` plus every fleet region — with `suggested` filled from the CDN country header (`cf-ipcountry` / `x-country`) through `[regions] country_map`; `[regions] echo_listen` runs a UDP echo on the node for the probes. The client puts its measurements into the matchmaker ticket as `ping_<region>` numeric properties (or a `region` string); the queue picks the region with the lowest worst ping and a dedicated server is allocated with `allocate_routed`: among the fleets named by the ticket's `fleet` property — a fleet name or a fleet `pool` — the match region first, then the rest. `POST /v1/server/allocate {pool, region?, payload, backfill?, wait_secs?}` does the same for game servers; with `wait_secs` (also on `/v1/server/fleets/{id}/allocate`) an empty pool cold-starts a server in the best fleet with room and waits for it, so fleets can run `min_servers = 0, buffer = 0` and scale to zero. Providers `edgegap` (`[fleet.edgegap] token, app, version`; a fleet `spec.image` of `app:version` overrides) and `gamelift` (`[fleet.gamelift] access_key, secret_key, region, fleet_id|alias_id`) start and stop servers through the vendors' APIs like the built-in local/docker/firecracker/kubernetes ones.

### Realtime transport: channels and compression

**Channels.** Every transport speaks the same four names: `reliable_ordered`, `reliable_unordered`, `unreliable_sequenced`, `unreliable`. `match_data_send {channel: "unreliable_sequenced"}` picks one (it overrides the older `reliable` flag); the frame arrives as `match_data {channel, seq}` — unreliable frames carry `seq` (per match, per session, from 0), so a client drops a frame whose `seq` is below the last applied. Transports without the channel degrade: WebSocket delivers everything reliable_ordered, QUIC/WebTransport/WebRTC send unreliable frames as datagrams / unordered channels, raw UDP uses its channel 1. The labels stay honest either way.

**Capabilities.** `transport {}` replies `transport {transport: "ws"|"quic"|"webtransport"|"webrtc"|"udp", channels: [native channels], max_frame_bytes, compression: ["lz4"], compressed}` — ask once after connecting and decide how to pack state.

**lz4.** On the binary (`postcard`) wire, `transport {compress: "lz4"}` switches the session to per-frame compression both ways: every binary frame from then on is a 1-byte tag — `0` raw, `1` `lz4_flex` block with the uncompressed size prepended (`musubi_proto::lz4_wrap / lz4_unwrap`) — and payloads over 256 bytes that actually shrink go compressed. JSON sessions get `invalid_argument`. A 2 KB repetitive delta lands in well under 300 bytes.

### Developer tools

**Module canaries.** `POST /v1/admin/runtime/modules/{name}/canary?percent=10&accounts=a,b` uploads a second build of a module: RPCs of the pinned accounts and a stable `percent` of all accounts run on it, everyone else stays on the active build, on every node (the canary travels with the usual reload broadcast). `POST …/canary/promote` makes it the active build, `DELETE …/canary` drops it; every upload is a row in `GET …/versions` (`active|canary|retired`, sha256, note) and `POST …/versions/{v}/rollback` re-activates an earlier one.

**Request recording and replay.** `PUT /v1/admin/dev/recording {enabled, sample_percent?, path_prefix?}` keeps sampled requests with their responses (`GET /v1/admin/dev/recordings?path=`, `GET …/{id}`; secrets are stripped, `/v1/admin/dev/*` and replays are never recorded). `POST …/{id}/replay` re-issues the request on this node — as the server (current key) or as the recorded account (a 5-minute token) — and reports whether the status matches.

**Fixtures.** `POST /v1/admin/dev/fixtures {name, accounts: [{username, display_name?, metadata?, wallet?: {gold: 100}, inventory?: [{item_id, quantity}], storage?: [{collection, key, value}]}], reset?}` creates the accounts once (re-applying finds them again), tops wallets up idempotently and seeds inventory and player data for new accounts (or all with `reset`); `DELETE /v1/admin/dev/fixtures/{name}` purges them.

**CPU profiles on demand.** `GET /v1/admin/debug/profile?seconds=10&hz=99&format=flamegraph|pprof|folded` samples this node for `seconds` (1..=120, `SIGPROF` at `hz`) and answers with a flamegraph SVG, a `pprof` protobuf (`go tool pprof`, Pyroscope, speedscope) or folded stacks (`thread;frame;frame count` per line, for inferno/speedscope). Same guards as `/metrics` (`[metrics] allow_ips`, server key), one profile at a time per node (`409` otherwise), Linux only. Point it at the node that serves a hot match or a slow RPC instead of restarting under `perf`.

**Deploy.** `Dockerfile` builds the server image; `deploy/helm/musubi` is a Helm chart (TOML config in a ConfigMap, secrets as `MUSUBI__*` env, Service for HTTP and the UDP edge, HPA and PDB). `deploy/terraform/musubi` wraps the chart as a Terraform module (namespace, Secret with database URL / server key, ConfigMap TOML, HPA/PDB, optional UDP edge Service) for EKS/GKE/AKS/k3s — Postgres comes from the caller (`database_url`).
