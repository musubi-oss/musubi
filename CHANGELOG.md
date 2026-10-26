# Changelog

All notable changes to Musubi. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow SemVer with the `0.x` caveat that shapes may change between minor versions until `1.0`.

## Unreleased

### Unreal GDK
- Cloth on a handed-over actor gets `ForceClothNextUpdateTeleportAndReset` after the pose is restored (no explosion).
- `IMusubiHandoverState`: components add their own state to `musubi.phys.ext` across a handover; new optional
  `MusubiVehicles` module carries the Chaos wheeled vehicle snapshot (gear, RPM, wheels) with it.
- `musubi.phys.bodies` is a quantised binary blob (base64; ~45 B + name per body, ~4× smaller than the JSON arrays,
  which are still read).
- Uncooked servers no longer close clients that stream a World Partition cell the server lacks:
  `net.SkipMissingLevelDisconnect` is set for them (`bSkipMissingLevelDisconnectUncooked`); the WP NITL client now
  streams by its own controller.
- Handover release wait is adaptive: the node waits for the owner's release up to 10 ticks while the owner keeps
  renewing its leases, 2 ticks once it went silent.
- Handover protocol: the node asks the receiving simulator only after the owner released the entity (its final
  `apply` + `handover_ack`, or 2 ticks), so the receiver resumes from the owner's last state; the GDK no longer
  captures physics from its ghost on `handover_request`, and restores it on the ownership flip of a local handover
  (previously only the cross-node `handover_arrived` path did).
- Ragdolls across the border: `musubi.phys.bodies` carries every simulating body beyond the root (each bone of a
  `USkeletalMeshComponent`, every simulating primitive) — pose relative to the root, velocities, awake flag,
  quantised — and broken skeletal constraints; the new owner re-enables the bodies and applies them with
  `SetBodyTransform` + velocities, so a ragdoll keeps falling instead of resetting. `NITL_RAGDOLL=1` covers it.
- Multi-map worlds without travel: `WorldConfig::levels` (`"x,y"` → map asset, `"*"` default) names a level per cell; simulators stream their leased cells + the ring around them and clients the cells around their pawn (`UMusubiNetDriver::CellLevelRadius`) as level instances of one shared persistent map (`FMusubiCellLevelStreamer`), so a handover or a rehome never changes maps. `world_joined` now carries `cell_size` and `levels`; `UMusubiSimulatorSettings::CellLevels` stays as the fallback. NITL: `NITL_LEVELS=1`.
- Client rehome: `GuidTableTimeout` (how long `NMT_Join` waits for the new owner's NetGUID table so the kept actors are reused instead of swapped) defaults to 3 s instead of 1 s — a server that has just adopted the entity after a failover answers late. NITL chaos mode (`NITL_CHAOS=1`) is green again: the second client now connects before the kill, the harness no longer dies on an empty log window, and it cleans up `nitl-c2`/`nitl-s2b`.
- Iris on the relay driver (5.7+): with `bCanUseIris=true` + `net.Iris.UseIrisReplication=1` the client ↔ owning-server driver replicates through the engine's Iris bridge, and an in-place rehome keeps the client's objects — `UMusubiActorFactory`/`UMusubiSubObjectFactory` (engine factories + entity id) hand the new owner's pawn, PlayerController, PlayerState and dynamic subobjects to the instances the client already has instead of spawning twins; the client's `UReplicationSystem` restarts per owner (`RestartIrisSystem`). Set as the bridge's default factories by the plugin config (opt-in per actor via `FillReplicationParams` → `UMusubiEntityRegistry::IrisNetFactoryName`). `NITL_IRIS=1` runs the harness on Iris (docs/gdk-unreal.md 9.2).
- World Partition streaming per cell: a dedicated server is a WP streaming source covering its leased cells (+ `ghost_margin` or `CellStreamingMargin`), so one partitioned map serves the whole world — the engine loads/unloads WP cells under lease changes, split/merge and handover, and a rehome never changes the client's map. `MusubiServer` turns on `wp.Runtime.EnableServerStreaming(+Out)` on `-server` processes, player controllers are filtered out of the server's sources (`bIgnorePlayerStreamingSources`); `bWorldPartitionStreaming = false` restores the old behaviour. `UMusubiSimulatorSubsystem::LeasedCells()` now lists leased cells only (ghost-only cells no longer appear); `WorldCellSize()` / `CellBounds()` expose the grid. NITL: `NITL_WP=1` builds a partitioned map headless (`UMusubiNitlWpMapCommandlet`) and checks the streamed-in cells on both servers.
- Ghosts (entities of neighbouring cells) now live on the server as `ROLE_SimulatedProxy`: `HasAuthority()` is false for every Blueprint and third-party plugin, CharacterMovement runs in simulated mode, Server RPCs called on a ghost are routed to its owner, and they still replicate to this server's clients as simulated proxies (`UNetDriver::SetRoleSwapOnReplicate`; 5.8+: `EnableReplicationOfActorsWithAnyRole`). Roles flip on handover in both directions. Opt out with `UMusubiSimulatorSettings::bGhostsAsSimulatedProxy=false`; kept as authority on an Iris GameNetDriver before 5.8 (the engine bridge cannot replicate non-authority actors there). NITL checks the roles on both servers.
- Relayed engine connections now run the engine's `PacketHandler` chain: `UMusubiNetConnection::InitHandler` builds it like `UNetConnection::InitHandler` with `FMusubiRelayHandshakeComponent` in place of the stateless-connect cookie exchange (the node has already authenticated both ends), so `[PacketHandlerComponents]` (AES-GCM encryption, Oodle compression) apply to Musubi connections and the engine's `EncryptionToken` handshake works, including a re-key by the new owner on an in-place rehome (`NMT_EncryptionAck` handled by the driver). NITL: `NITL_ENCRYPT=1`.

## 0.2.0 — 2026-10-25

Full backend security and modernization audit: every crate, every file of the node and all 220 migrations were reviewed against one checklist (authn/authz per route, IDOR, constant-time secret comparison, token entropy, SQL, races, limits, SSRF, panics, crypto, leakage). Highlights below; the per-area detail is in the commit history (`audit(<area>)`).

### Security
- **Node bus:** QUIC connections between nodes now require mutual TLS bound to the node's `bus_cert`; previously any client that could reach the bus port could inject SWIM verdicts, Raft RPCs and catalog writes.
- **Money / rewards:** ad reward completion, tutorial rewards, donation requests, `buy_shield`, sweepstake entries, grant limits, code redemptions and other check-then-act paths are now atomic (advisory locks, single-statement updates, checked arithmetic); duplicate payouts and overflow-into-credit are closed.
- **Identity:** login-queue tickets are no longer accepted as access tokens; restricted server keys cannot mint tokens with wider scopes; refresh / TOTP / MFA / reset flows are single-use and rate limited; passkey CBOR parsing is bounded; TOTP secrets and recovery codes are sealed / stronger; reset tokens no longer leak into LiveOps and analytics events.
- **Operators:** SSO `return_to` is restricted to the console origins (open redirect leaked a fresh operator key); `GET /v1/admin/config` redacts secrets inside arrays; invite tokens are stored hashed.
- **Input:** SQL injection through nested audience-rule `field`, private-lobby code bypass, path traversal and archive limits in content / builds, recursion and size limits in parsers and protocol frames, body and pagination caps on all list routes.
- **Egress:** webhooks, push, config sources, environments and entitlement sync go through one SSRF guard (no private / link-local / metadata targets, no redirects, timeouts).
- **Client address:** `X-Forwarded-For` / `X-Real-IP` are believed only from `[http] trusted_proxies` (default: loopback and private ranges); rate limits, bans and geo can no longer be spoofed by the client.
- All key / token / signature comparisons are constant-time; one-time tokens come from a CSPRNG (256 bit) instead of UUID pairs.

### Changed
- **BREAKING (config):** with `session.signing_key_pem` (ES256) and `[[projects]]`, each project needs its own `[projects.session] signing_key_pem` (or an explicit `null` to use HS256 with the derived secret); the base key was inherited, so a token for project A verified for project B.
- Consoles served from an origin other than the SSO `redirect_url` must be listed in `[http] cors_origins` to be accepted as `return_to`.
- Error texts of several auth failures are more specific (same codes and JSON shape).
- Dependencies: `once_cell` → `std::sync::LazyLock`, `async_trait` → native async traits where object safety allows, `rustls-webpki` / `webpki-roots` updated.
- `cargo audit` runs in CI; the accepted advisories (crypto-bigint / rsa via `jsonwebtoken` verification paths, quick-xml via flamegraph writing, atomic-polyfill on targets without atomics) are listed with reasons in `.cargo/audit.toml`. `pprof` 0.14 → 0.15.
- Unreal GDK: generated wire readers reject unknown optional tags and pre-reserve at most 1024 elements per array (the length prefix is no longer trusted for allocation). Regenerate with `cargo run -p musubi-unreal-gen` after pulling.

### Database
- `0221` / `0222`: indexes behind the retention sweeper and the audit fixes; forward-only as before.

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
