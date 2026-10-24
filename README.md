# Musubi

[![ci](https://github.com/musubi-oss/musubi/actions/workflows/ci.yml/badge.svg)](https://github.com/musubi-oss/musubi/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/musubi-oss/musubi?include_prereleases)](https://github.com/musubi-oss/musubi/releases)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**Open-source game backend in Rust with server meshing.** One binary gives you accounts and
authentication, player data, social and chat, leaderboards and tournaments, matchmaking,
authoritative realtime, dedicated-server fleets, economy and LiveOps, an operator console and —
the part nobody else ships open — a **seamless persistent world** spread over many simulators
and many machines, with handover and failover that do not drop the player.

Apache-2.0. Clustered out of the box. Postgres is the only dependency.

```
clients (HTTP / WebSocket / QUIC)          Unreal dedicated servers (Musubi GDK)
            │                                            │
            ▼                                            ▼
┌──────────────────────────────────────────────────────────────────────┐
│  musubi-server  ×N   roles: edge · api · realtime · sim · orchestrator│
│  ───────────────────────────────────────────────────────────────────│
│  auth · accounts · storage · social · chat · leaderboards · parties   │
│  matchmaker · matches · worlds (cells, leases, handover, interest)    │
│  fleets (docker / k8s / firecracker / GameLift / Edgegap / webhook)   │
│  economy · LiveOps · WASM modules · webhooks · console at /console    │
└──────────────┬──────────────────────────────┬────────────────────────┘
               │ node bus: QUIC + Raft         │ or Postgres LISTEN/NOTIFY
               ▼                              ▼
                         PostgreSQL 16 (one database)
```

## Why

Every piece of a multiplayer backend exists somewhere — but as a hosted service with a bill,
as a framework that stops at "run your own server", or as an engine-locked meshing product.
Musubi puts the whole thing in one open binary:

- **Seamless worlds that scale out.** A world is a grid (or octree) of cells; cells are leased
  to simulators, split when hot, merged when cold and moved between nodes automatically.
  Entities cross cell and node borders with a handover protocol (ghosts, hysteresis, buffered
  input, journal), clients only ever see one continuous world. Measured: 2 000 clients on
  4 worlds × 4 000 entities across 2 nodes at 25–28 frames/s per client
  ([numbers](docs/operations.md#measured-not-promised)).
- **Failover that keeps sessions.** Nodes are killed every 12 s in the nightly chaos stand;
  worlds re-home from snapshots + journals, resumable WebSocket sessions reconnect, zero lost
  writes is the pass condition ([`musubi soak`](docs/operations.md#chaos-stand-musubi-soak)).
- **Unreal Engine first-class.** A NetDriver-level plugin: the player's pawn, PlayerController,
  PlayerState and replicated subobjects stay the *same instances* while authority moves to
  another dedicated server. Join by `musubi://<world>` — no manual entity ids
  ([GDK](docs/gdk-unreal.md)).
- **Everything a live game needs on day one** — the list below is implemented and tested, not a
  roadmap. 1 077 HTTP paths, 220 migrations, 650+ tests run against Postgres on every push.
- **Cheap to run.** One Rust process per node, ~200 MB image, no sidecars, no JVM, no Redis.
  5 400 HTTP req/s and 35 000 WebSocket messages/s per 8-vCPU node in the bundled bench.

## What is inside

| Area | Features |
|---|---|
| **Identity** | device / e-mail+password (argon2id) / magic link / custom; Steam, Apple, Google, Game Center, Facebook, Xbox, PlayStation, Nintendo link & unlink; JWT with key rotation, refresh-token families, device-code flow, session caps, brute-force locks, OAuth2 clients, SSO for operators, age gate & parental controls, GDPR data rights (export / delete / tombstones) |
| **Players** | profiles, metadata, bans and sanctions with appeals, notes, audit trail, segments, geo & attribution, crash reports, kick / silence / shadow, dormant-account sweeps |
| **Data** | typed storage collections with contracts, JSON Patch, aggregates, full-text search, indexes, live object subscriptions, versioned content bundles per environment, schema migrations, one IDL → contracts + RPC validation + OpenAPI + Rust/C++ types |
| **Social** | friends, blocks, groups with roles, parties with invites and join requests, presence with rich status, notifications (TTL, templates, schedule, Web Push, e-mail), chat (rooms / groups / direct, history, typing, attachments, automod, filters) |
| **Competition** | leaderboards (seasons, buckets, records, around-me), player statistics, tournaments with brackets (single / double elimination, round robin, standings, forfeit), ratings with seasons and decay, achievements, quests with pools and rerolls, progress graphs |
| **Realtime** | WebSocket + QUIC/WebTransport + UDP channels, JSON or postcard framing, LZ4, resumable sessions, relayed and authoritative matches, turn-based matches, streams, rollback match support, interest management |
| **Matchmaking** | skill / latency / party-aware queues, ready check, backfill, pause / drain, MMF testing and simulation, queue observability, georouting |
| **Servers** | fleets with builds (delta upload), autoscaler rules, canaries; providers: Docker, Kubernetes, Firecracker (snapshots, warm pool), AWS GameLift, Edgegap, webhook; allocation, health, logs, crash capture |
| **World meshing** | named worlds at 30 Hz, 2-D grid or 3-D octree cells, simulator leases, split / merge, placement strategies (adjacency, points of interest, player density, custom hook), cross-node offload, handover journal, time travel (snapshot / verify / rewind), persistent entities, entity ownership by account, `world_join` → find-or-spawn |
| **Economy** | currencies and wallets with an append-only ledger, items (stackable / unique / bound / equipable), inventory limits, store with per-platform listings, reward tables and modifiers, battle pass / progression tracks, seasons, crafting, auctions, trades, code campaigns with fraud flags, IAP validation, subscriptions |
| **LiveOps** | feature flags, experiments, remote config, audiences, live events, offers, news, changesets with approval, environments with promotion and dry-run, scheduled jobs |
| **Logic** | WASM modules (wasmtime) with publish / canary / promote / rollback, RPCs, hooks (auth, storage, matchmaker placement, …), durable workflows (sagas), webhooks with retries and deliveries |
| **Analytics** | event ingestion with schemas and sampling, sinks, KPI (DAU/WAU/MAU, retention cohorts, funnels, revenue), daily reports |
| **Operations** | console at `/console` (React + TanStack, embedded in the binary), Prometheus `/metrics`, OpenAPI at `/openapi.json`, retention policies, encrypted secrets with re-keying, `musubi doctor`, Helm chart and Terraform (Hetzner, OVH), Nakama / PlayFab / UGS importers |

Per-subsystem detail: [docs/features.md](docs/features.md). API by area: [docs/http-api.md](docs/http-api.md).

## Quick start

### 1. Run a node

With Docker (image from GitHub Releases):

```sh
docker network create musubi
docker run -d --name musubi-pg --network musubi \
  -e POSTGRES_USER=musubi -e POSTGRES_PASSWORD=musubi -e POSTGRES_DB=musubi postgres:16-alpine
docker run -d --name musubi --network musubi -p 7350:7350 \
  -e MUSUBI__DATABASE__URL=postgres://musubi:musubi@musubi-pg:5432/musubi \
  -e MUSUBI__HTTP__SERVER_KEY=change-me \
  -e MUSUBI__SESSION__SECRET="$(openssl rand -hex 32)" \
  ghcr.io/musubi-oss/musubi:latest
```

The node refuses to start on built-in secrets: `MUSUBI__SESSION__SECRET` (or `session.signing_key_pem` for ES256) and the server key are required; `musubi doctor` lists everything else it wants before production.

Or from source (Rust 1.85+):

```sh
docker run -d --name musubi-pg -e POSTGRES_USER=musubi -e POSTGRES_PASSWORD=musubi \
  -e POSTGRES_DB=musubi -p 5432:5432 postgres:16-alpine
cargo run -p musubi-cli -- dev     # all roles in one process, migrates on start, key `defaultkey`
```

Migrations run on start. `GET /healthz`, `GET /readyz`, `GET /v1/node` tell you it is up.

### 2. Open the console

`http://localhost:7350/console/` — sign in with the server key (or create an operator with an
e-mail and password under *Settings → Operators*, invite others, enable SSO). Dashboard,
players, worlds, fleets, matchmaking, economy, LiveOps, analytics and every admin setting live
there; *Settings → Developer tools → Load demo data* fills a fresh node with players, currencies
and items.

### 3. Talk to the API

```sh
# a player
curl -s localhost:7350/v1/auth/device -H 'content-type: application/json' \
  -d '{"id":"my-device-id-0001"}'                 # → access_token / refresh_token
# your game server
curl -s localhost:7350/v1/server/worlds -H 'x-musubi-server-key: change-me' \
  -H 'content-type: application/json' -d '{"name":"demo"}'
```

Full schema: `GET /openapi.json`. Server-to-server keys with roles are issued with
`POST /v1/admin/keys` and revoked instantly cluster-wide.

### 4. Plug in Unreal

Copy `sdk/unreal/Musubi` into `Plugins/`, point `DefaultGame.ini` at the node, derive your
GameMode from `AMusubiGameMode`, put `UMusubiAuthorityComponent` on the player character, and
clients connect with `open musubi://demo`. Dedicated servers run `-server -MusubiCells=…` (or
let the node assign cells). Step by step: [sdk/unreal/README.md](sdk/unreal/README.md) and
[docs/gdk-unreal.md](docs/gdk-unreal.md). Engine 5.3 → 5.8 (Iris bridge on 5.5+).

## Configuration

TOML or YAML via `--config` / `MUSUBI_CONFIG`; every key can be overridden with
`MUSUBI__SECTION__KEY` (`MUSUBI__DATABASE__URL`, `MUSUBI__HTTP__LISTEN`, …). No config file is
needed to start. `musubi config` prints the effective configuration, `musubi doctor` checks
it (built-in secrets are refused outside `musubi dev`).

| key | default | meaning |
|---|---|---|
| `node.roles` | all | `edge`, `api`, `realtime`, `sim`, `orchestrator` — any subset per process |
| `node.bus_listen` | empty | QUIC node bus (Raft directory); empty = Postgres LISTEN/NOTIFY bus |
| `http.listen` | `0.0.0.0:7350` | HTTP, WebSocket and console |
| `http.server_key` | — | bootstrap operator / server key |
| `database.url` | — | Postgres 16 |
| `quic` / `edge` | off | QUIC/WebTransport and the UDP edge on `7351` |

Every other key is documented next to the feature it drives in [docs/features.md](docs/features.md) and [docs/operations.md](docs/operations.md).

## Deploying a cluster

Run several `musubi-server` processes on one database; they discover each other through the
database (or the QUIC bus), elect a directory leader, lease worlds and cells, and drain on
`SIGTERM`. Ready-made:

- **Helm**: [`deploy/helm/musubi`](deploy/helm/musubi) — replicas, HPA, PDB, secrets as
  `MUSUBI__*`, optional UDP edge LoadBalancer.
- **Terraform**: [`deploy/terraform`](deploy/terraform) — module plus Hetzner and OVH stacks.
- **Single host**: the Docker image above behind any reverse proxy; the stand we test against
  is a Coolify service with exactly that compose.

Operations (export / import, migrations from Nakama / PlayFab / UGS, retention, keys,
`musubi deploy` manifests, benchmarks, chaos stand): [docs/operations.md](docs/operations.md).

## Repository layout

| path | what |
|---|---|
| `crates/musubi-server` | the node: HTTP/WS/QUIC API, realtime, worlds, fleets, console embedding, migrations |
| `crates/musubi-cli` | `musubi` — `dev`, `migrate`, `doctor`, `export`/`import`, `migrate-nakama`/`-playfab`, `module`, `fleet`, `deploy`, `bench`, `soak` |
| `crates/musubi-mesh` | server-meshing core: cells, leases, handover, interest, controller, snapshots |
| `crates/musubi-cluster` | membership, Raft directory, placement, leases, node bus |
| `crates/musubi-actor` | actors with journal, snapshots and failover |
| `crates/musubi-transport`, `musubi-edge`, `musubi-quic-ffi` | WebSocket / QUIC / WebTransport / UDP, ingress, C-ABI QUIC for the Unreal plugin |
| `crates/musubi-auth`, `-account`, `-storage`, `-social`, `-chat`, `-leaderboard`, `-matchmaker`, `-economy`, `-fleet`, `-runtime`, `-integrations`, `-idl`, `-proto`, `-core` | the subsystems |
| `crates/musubi-sim-sdk` | Rust SDK for writing a simulator (`run_sim_stream`) |
| `crates/musubi-unreal-gen` | generates the C++ wire layer from the Rust protocol (`--check` in CI) |
| `console2/` | operator console: React 19, TypeScript, Vite, TanStack Router/Query/Table, shadcn/ui; typed client generated from the node's OpenAPI |
| `sdk/unreal/` | Unreal plugin (client, world view, server module, Iris bridge, NetDriver) and demo project |
| `deploy/` | Helm chart, Terraform |
| `docs/` | GDK design, feature and API references, operations, competitive analysis (22 teardowns) |

## Developing

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
MUSUBI_TEST_DATABASE_URL=postgres://musubi:musubi@localhost:5432/musubi cargo test --workspace
cd console2 && pnpm install && pnpm build && pnpm test     # typecheck + OpenAPI path check + vitest
```

The console build is embedded into the server binary (`console/dist`, `include_dir!`), so a
console change is `pnpm build`, copy `console2/dist` → `console/dist`, `cargo build`. CI runs the
Rust suite against Postgres, the console build and tests, the Unreal wire-layer checks and
criterion benches against the base commit; a nightly chaos stand kills nodes for 20 minutes
and fails on any lost write.

## Status

`0.1.x` — alpha. The API is complete enough to run a game on, the protocol and the wire are
pinned by tests, but we do change shapes between minor versions until `1.0`; migrations are
always forward-compatible. Things we know are not done: client SDKs other than Unreal (Unity
and Godot are next, the OpenAPI schema and the generated wire layer are the starting point),
multi-region active/active for one world, and a hosted build of the console outside the node.

Comparison with Nakama, PlayFab, Pragma, AccelByte, Photon, SpatialOS-style meshing vendors and
17 others: [docs/competitive-analysis.md](docs/competitive-analysis.md),
[docs/parity.md](docs/parity.md).

## License

Apache-2.0 — see [LICENSE](LICENSE).
