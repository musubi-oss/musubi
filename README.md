# Musubi

Open-source game backend in Rust: accounts, data, social, chat, leaderboards, matchmaking,
authoritative realtime, dedicated-server fleets and **server meshing** — one Apache-2.0 binary,
clustered out of the box.

Status: pre-alpha (M0 — foundation). Not usable yet.

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
| `GET /v1/leaderboards[/{id}]`, `GET\|POST\|DELETE /v1/leaderboards/{id}/records`, `GET /v1/leaderboards/{id}/around/{owner}` | bearer | leaderboards: best/set/incr/decr operators, asc/desc, cron reset windows, 1-based ranks, owner lookup, around-owner paging |
| `POST /v1/server/leaderboards`, `DELETE /v1/server/leaderboards/{id}`, `POST\|DELETE /v1/server/leaderboards/{id}/records/{owner}` | `X-Musubi-Server-Key` | server-to-server: create/delete boards, authoritative writes with operator override |
| `GET /v1/tournaments[/{id}]`, `POST /v1/tournaments/{id}/join`, `GET\|POST /v1/tournaments/{id}/records`, `GET /v1/tournaments/{id}/around/{owner}`; server: `POST /v1/server/tournaments`, `DELETE …/{id}`, `POST …/{id}/records/{owner}` | bearer / server key | tournaments: start/end, active duration per reset window, max_size, max_num_score, join_required, category range listing |
| socket `matchmaker_add` / `matchmaker_remove` → `matchmaker_ticket`, `matchmaker_matched` | token | matchmaker: mutual queries (`+field:value`, `-field:value`, `>=`, `[a TO b]`, `*`), min/max count, count_multiple, 15 s wait for max before settling for min |
| socket `party_create/join/leave/promote/accept/remove/close/join_request_list/data_send/matchmaker_add/matchmaker_remove` → `party`, `party_join_request`, `party_leader`, `party_presence_event`, `party_data`, `party_matchmaker_ticket`, `party_close` | token | parties: open or request-to-join, leader moderation, leader hand-off on leave, party-wide matchmaking tickets |
| socket `match_create`, `match_join` (by id or matchmaker token), `match_leave`, `match_data_send` → `match`, `match_data`, `match_presence_event` | token | relayed (client-hosted) matches, ≤64 players, targeted or broadcast data; authoritative matches come with the actor runtime |
| `GET /v1/matches?label=&min_size=&max_size=`; server: `POST /v1/server/matches {module, params}`, `GET\|DELETE /v1/server/matches/{id}`; socket `match_join/leave/data_send` on an authoritative id | bearer / server key | authoritative matches as actors (`musubi-actor`): handler over an opaque state blob (init/join_attempt/join/leave/tick/terminate), tick 1–60 Hz, label ≤ 2 KB; built-in `echo` module; snapshots in `musubi.match_snapshots` on every membership change / every 50 ticks, claimed atomically and resumed on startup and whenever a node stops heartbeating (failover); match modules in WebAssembly: exports `match_<module>_{init,join_attempt,join,leave,tick,terminate}` (JSON in/out, state as base64) are registered automatically |
| `POST /v1/rpc/{id}`, `GET /v1/rpc`; socket `rpc` | bearer or server key | server logic: sandboxed WebAssembly modules from `runtime.modules_dir` (fuel, memory cap, wall-clock deadline, hot reload per module); ABI v0 documented in `musubi-runtime`; hooks `before_rpc_<id>` / `after_rpc_<id>` / `hook_authenticated` |
| `GET /v1/server/cluster/nodes` | server key | cluster v0: node registry with heartbeats + Postgres LISTEN/NOTIFY bus; chat, notifications, status/presence events and matchmaker tickets are replicated across nodes (the lowest live node id runs matchmaking passes); parties and relayed matches live on their home node and members on any node reach them over the bus (matchmaker-formed relayed matches are homed on the leader) (gossip/Raft/QUIC bus replace this behind the same API) |
| `GET\|POST /v1/server/fleets`, `GET\|DELETE /v1/server/fleets/{id}`, `POST …/{id}/scale\|allocate`, `GET …/{id}/servers`, `GET\|DELETE /v1/server/gameservers/{id}`; SDK (server key + `X-Musubi-Server-Id`): `GET /v1/gameserver`, `POST /v1/gameserver/{ready,health,allocate,shutdown,label,connect}` | server key | dedicated-server orchestration v0 (`musubi-fleet`): fleets with buffer/max/port range, reconciler every 5 s (reaps dead/unhealthy, tops up to buffer), `local` process provider and `docker` provider (Docker CLI, `spec.image`, port published UDP+TCP), env `MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY`, group connect tokens; tickets with string property `fleet=<name>` get a server allocated and delivered in `matchmaker_matched.server`; kubernetes/agones/edgegap providers next |
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

## Integrations (Zanshin LiveOps, Aurix voice)

`[zanshin] url, server_key, flush_secs` and `[aurix] url, api_key, party_channel_type, match_channel_type` in the config (unset `url` = disabled). The Musubi account id is the `external_id` on both sides.

- Zanshin: authentication is tracked as `musubi.authenticated` (batched `POST /server/v1/events`); clients evaluate flags / live events for themselves via `GET /v1/liveops/flags?names=` and `GET /v1/liveops/live-events` (proxied with the server key, never exposed).
- Aurix: `POST /v1/voice/token {party_id | match_id, display_name?, speak?}` checks membership on this node and mints a player token for the ad-hoc channel `party-<id>` / `match-<id>` (created on first join, removed when empty); the response carries Aurix's `token`, `user_id`, `expires_at`, `channel_id`, `endpoint`.

## Economy v0 (`musubi-economy`)

Currencies with optional caps; wallets with an append-only ledger (`delta`, `balance_after`, `reason`, `ref`, idempotency key — a replayed key returns the original entry, balances never go negative); item catalog (stackable with `max_stack` or unique instances with `instance` JSON); store listings priced in several currencies with `max_per_account`, purchase = debit + grant in one transaction, idempotent. Players: `GET /v1/wallet`, `GET /v1/wallet/ledger?currency=&limit=`, `GET /v1/inventory`, `POST /v1/inventory/consume {id, quantity}`, `GET /v1/store`, `POST /v1/store/buy {store_item_id, currency, idempotency_key?}`. Server key: `GET|POST /v1/server/currencies`, `POST /v1/server/wallet/adjust {account_id, currency, delta, reason, ref, idempotency_key, metadata}`, `GET|POST /v1/server/items`, `POST /v1/server/inventory/grant`, `GET|POST /v1/server/store`. Rewards tables, progression, battle pass, auctions and IAP validation are next.
