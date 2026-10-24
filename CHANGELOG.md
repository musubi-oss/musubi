# Changelog

All notable changes to Musubi. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow SemVer with the `0.x` caveat that shapes may change between minor versions until `1.0`.

## 0.1.0 — 2026-10-24

First public release: one Rust binary with the full backend, the operator console embedded, the Unreal GDK, Helm/Terraform and the importers.

### Platform
- `musubi-server` node with roles `edge`, `api`, `realtime`, `sim`, `orchestrator`; clusters on one Postgres 16 through the QUIC + Raft bus or Postgres LISTEN/NOTIFY; drains on `SIGTERM`; 220 forward-only migrations applied on start.
- `musubi` CLI: `dev`, `migrate`, `doctor`, `config`, `export` / `import`, `migrate-nakama`, `migrate-playfab` (PlayFab + Unity Gaming Services), `module`, `fleet`, `build`, `schema`, `idl`, `deploy`, `bench`, `soak`.
- Configuration from TOML/YAML or `MUSUBI__SECTION__KEY` environment variables; no config file required to start.
- OpenAPI at `/openapi.json` (1 077 paths), Prometheus `/metrics`, `/healthz`, `/readyz`.

### Server meshing
- Named worlds at 30 Hz, 2-D grid or 3-D octree cells, simulator leases, split / merge, placement strategies (adjacency, points of interest, player density, custom module hook), automatic cross-node offload, orphaned-cell recovery.
- Entity handover with ghosts, hysteresis, buffered input, collision rules and a durable handover journal; snapshots, verification and rewind (time travel); persistent entities restored across node loss.
- Entity ownership by account enforced on `net_open` / `world_input` / relay; `world_join` finds the account's entity or spawns it from the world's `player_spawn` policy.
- Streaming simulator protocol (full and delta frames) and the Rust `musubi-sim-sdk`.

### Game services
- Identity: device, e-mail + password (argon2id), magic link, custom, device-code flow; Steam, Apple, Google, Game Center, Facebook, Xbox, PlayStation, Nintendo linking; JWT key rotation, refresh-token families, session caps, brute-force locks, OAuth2 clients, operator SSO, age gate and parental controls, GDPR data rights.
- Accounts, profiles, segments, bans and sanctions with appeals, notes, audit, crash reports, geo and attribution.
- Storage collections with contracts, JSON Patch, aggregates, full-text search, live object subscriptions, content bundles per environment, IDL-driven schemas.
- Friends, blocks, groups, parties, presence, notifications (TTL, templates, schedule, Web Push, e-mail), chat with history, typing, attachments and automod.
- Leaderboards, statistics, tournaments with brackets, ratings with seasons and decay, achievements, quests and pools, progress graphs.
- Realtime over WebSocket, QUIC/WebTransport and UDP with JSON or postcard framing, LZ4, resumable sessions; relayed, authoritative and turn-based matches; streams.
- Matchmaking queues with ready check, backfill, pause / drain, MMF testing and simulation.
- Fleets with builds, autoscaler rules and canaries on Docker, Kubernetes, Firecracker, AWS GameLift, Edgegap and webhook providers.
- Economy: currencies, wallets with ledger, items, inventory, store, reward tables, progression, seasons, crafting, auctions, trades, code campaigns, IAP validation, subscriptions.
- LiveOps: flags, experiments, remote config, audiences, live events, offers, news, changesets with approval, environments with promotion.
- WASM modules (publish / canary / promote / rollback), RPCs, hooks, durable workflows, webhooks; analytics ingestion, KPIs and reports.

### Operator console
- React 19 + TypeScript + Vite + TanStack Router/Query/Table + shadcn/ui, served by the node at `/console`; typed client generated from the node's OpenAPI snapshot (checked by a test); URL-persisted filters, ⌘K search, cross-links by id, CSV export, desktop and mobile layouts; every resource with a `/{id}` route has a detail page.

### Unreal Engine GDK
- Plugin for UE 5.3 → 5.8: client facades (presence, storage, RPC, notifications, chat, matchmaker, parties, matches, streams), world view component, server module running headless Unreal as a simulator, Iris bridge (5.5+), NetDriver-level integration keeping pawn / PlayerController / PlayerState / replicated subobjects the same instances across handover, `musubi://<world>` join without manual entity ids.

### Tooling
- CI: fmt, clippy, full test suite against Postgres, console build and tests, Unreal wire-layer checks, criterion benches against the base commit; nightly chaos stand.
- Release: linux x86_64 / aarch64 tarballs and the multi-arch image `ghcr.io/musubi-oss/musubi`.
