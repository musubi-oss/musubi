# musubi — competitive analysis (22 competitor teardowns merged)

Scope: open-source Rust game backend **musubi** (https://github.com/musubi-oss/musubi, inspected read-only at commit `552e2f1`, 2026-10-18) against 22 competitor reports. Every report carried a feature inventory (`area | feature | competitor detail | source URL | musubi_status | musubi evidence`); this document merges all **4516 inventory rows** into one normalized matrix, a deduplicated gap backlog and a consolidated "where musubi leads" section. The flat per-(feature, competitor) data is in `musubi-competitive-matrix.csv`.

Status legend: ✅ have · 🟡 partial · ❌ missing · — n/a (not meaningful for a self-hosted Apache-2.0 backend: vendor billing, SLA tiers, cloud-internal mechanics). In the matrix a competitor column shows ✅ when that competitor's report lists the feature (its detail is in the CSV); the **musubi** column aggregates the merged rows' verdicts and shows the counts `h/p/m` (have/partial/missing rows).

## 1. Executive summary

### 1.1 Counts per competitor

| competitor | product | features total | have | partial | missing | n/a | "musubi ahead" rows | pages scanned |
|---|---|---|---|---|---|---|---|---|
| **AccelByte** | AccelByte Gaming Services (AGS) | 171 | 82 | 56 | 22 | 11 | 16 | 1,399 |
| **Agones/OM** | Agones + Open Match (Google for Games) | 218 | 64 | 79 | 47 | 28 | 22 | 167 |
| **Beamable** | Beamable | 266 | 99 | 94 | 36 | 37 | 24 | 467 |
| **brainCloud** | brainCloud (bitHeads) | 176 | 80 | 53 | 23 | 20 | 18 | 1,720 |
| **Colyseus** | Colyseus (OSS + Cloud + Arena) | 145 | 66 | 49 | 15 | 15 | 19 | 142 |
| **Edgegap** | Edgegap (Arbitrium, Private Fleets, Matchmaker Gen2, Server Browser, Relay, Managed Clusters…) | 176 | 67 | 59 | 33 | 17 | 15 | 81 |
| **EOS** | Epic Online Services | 134 | 37 | 50 | 19 | 28 | 20 | 228 |
| **GameLift** | Amazon GameLift (Servers, FlexMatch, Streams) | 277 | 59 | 110 | 40 | 68 | 15 | 628 |
| **Hathora** | Hathora Cloud (acquired by Fireworks AI; platform frozen, GameFabric transition) | 224 | 93 | 55 | 30 | 46 | 19 | 116 |
| **LootLocker** | LootLocker | 163 | 63 | 54 | 35 | 11 | 27 | 1,198 |
| **Meshing vendors** | Server-meshing vendors: coherence, Hadean, SpatialOS (archived), Flax netcode, Star Citizen | 240 | 134 | 59 | 23 | 24 | 16 | 629 |
| **Metaplay** | Metaplay | 201 | 47 | 73 | 56 | 25 | 21 | 333 |
| **Hiro/Satori** | Hiro (game systems on Nakama) + Satori (LiveOps) + Heroic Cloud | 154 | 33 | 72 | 39 | 10 | 23 | 131 |
| **Nakama** | Nakama server (OSS + Enterprise) + Console + Heroic Cloud | 180 | 117 | 39 | 9 | 15 | 25 | 167 |
| **Photon** | Photon (Fusion, Quantum, Realtime, PUN, Chat, Voice, Video, Server, Cloud, Tournament SDK) | 280 | 134 | 59 | 38 | 49 | 21 | 124 |
| **PlayFab** | Azure PlayFab (Core, Multiplayer Servers, Party, Lobby/Matchmaking, Economy v2, Experiments, Insights) | 270 | 124 | 64 | 56 | 26 | 16 | 2,772 |
| **Pragma** | Pragma Engine | 227 | 118 | 63 | 28 | 18 | 21 | 1,049 |
| **Rivet** | Rivet (rivet.dev) — actors/workflows orchestrator; game-backend product discontinued with v2.0 (2025-09) | 205 | 79 | 65 | 23 | 38 | 16 | 614 |
| **SpacetimeDB** | SpacetimeDB (Clockwork Labs) | 147 | 62 | 41 | 20 | 24 | 20 | 115 |
| **Steamworks** | Steamworks (Valve) backend/Web API features | 218 | 108 | 56 | 22 | 32 | 20 | 134 |
| **UGS LiveOps** | Unity Gaming Services — LiveOps (Authentication, Cloud Code, Cloud Save, Economy, Leaderboards, Remote Config, Friends, Analytics, Push, Game Overrides, Access Control, Triggers, Scheduler) | 205 | 111 | 64 | 14 | 16 | 29 | 706 |
| **UGS Multiplayer** | Unity Gaming Services — multiplayer (Sessions, Lobby, Matchmaker, Relay, Distributed Authority, QoS, Vivox, NGO/N4E, Multiplay → Rocket Science) | 239 | 85 | 66 | 40 | 48 | 13 | 1,123 |
| **All 22** | | **4516** | **1862** | **1380** | **668** | **606** | | **14,043** |

Counts are per inventory row after the verdict corrections listed in §5.3 (six rows changed). Rows bundle several sub-capabilities each, so "have" means the capability exists, not that every limit/tier matches.

### 1.2 Overall verdict

Across the merged inventory, 264 normalized feature groups absorb 2812 rows; 1704 rows stayed as single-competitor features. musubi's aggregated status over the normalized groups: **87 have, 112 partial, 62 missing** (groups that are all n/a excluded). The pattern is the same in every report:

- **Where musubi is structurally ahead:** the *server* — server meshing (cells, handover, ghosts, cross-cell transactions, time-machine), authoritative WASM match actors with snapshots/failover/rollback/spectators/replays, five realtime transports with channel classes and lz4, a 9-provider dedicated-server fleet layer, OSS clustering (SWIM + Raft), identity breadth (device/email/magic link/passkeys/TOTP/Steam/Epic-via-OIDC/Xbox/PSN/Nintendo/Apple/Google/Facebook/OIDC, merge, session management, login queue), economy depth (wallets/ledger, items with versions, store/bundles/coupons, IAP for 9 stores, subscriptions, marketplace/order book, trading, crafting, reward tables/modifiers, grant limits, fraud), progression (tracks, graphs, quests with rerolls, seasons, streaks, energy, group challenges), durable workflows/sagas, compliance (export/erase, retention, age gate + parental consent), dev tools (bench/soak/fixtures/request replay/profiles) — all in one Apache-2.0 binary with no CCU caps.
- **Where musubi is behind, in every single report:** (1) **no client SDKs** — only an Unreal scaffold, while every competitor ships typed clients for Unity/Unreal/TS/C++ and more; (2) **no admin console** — README says it is not written, every competitor has a web UI; (3) **no analytics** — no client telemetry ingestion, no KPI dashboards, no warehouse sinks (delegated to Zanshin/Prometheus); (4) **no A/B experiments, native feature flags/live events or campaign tooling** in core (Zanshin proxy only); (5) **WASM-only server scripting** where competitors offer JS/TS, Lua, C#, Go; (6) **dedicated-server operability** — no log capture, per-server metrics, resource/port specs, label-selector allocation, rolling updates, external match functions; (7) **no voice**, no P2P/NAT traversal or standalone relay for third-party netcode; (8) **trust & safety structure** — bans only (no sanctions/appeals, device bans, incidents), no legal-consent versioning, no hosted account pages.
- **Verdict:** musubi already out-builds the hosting-only vendors (Agones/Open Match, Edgegap, Hathora, GameLift, Rivet, SpacetimeDB, Photon, coherence) on game services and out-builds the BaaS vendors (PlayFab, Beamable, brainCloud, LootLocker, Metaplay, UGS, AccelByte, Nakama) on realtime/meshing/hosting — but it cannot be *adopted* until the SDK + console + analytics layer exists. The P0 backlog below is therefore dominated by developer experience and LiveOps tooling rather than by new gameplay services.

### 1.3 Verdict per competitor

**AccelByte** — AccelByte Gaming Services (AGS) (171 rows: 82 have / 56 partial / 22 missing / 11 n/a). **Behind:** AGS is a full publisher-grade suite — client SDKs for every engine/console, an Admin Portal, real-money commerce (orders, PSPs, tax, refunds), legal/consent versioning, first-party session sync, hosted analytics, OAuth2 clients with per-endpoint permissions and SAML. **Ahead:** musubi ships server meshing, authoritative WASM match actors, multi-transport realtime, in-process modules with canary/rollback and durable workflows, all self-hosted under Apache-2.0 — none of which AGS offers (AMS/Extend are hosted and billed).

**Agones/OM** — Agones + Open Match (Google for Games) (218 rows: 64 have / 79 partial / 47 missing / 28 n/a). **Behind:** Agones/Open Match are deeper on the DS/matchmaking *mechanics*: native DS SDKs with a push watch stream, pluggable external match functions, label-selector allocation with fallbacks, Counters/Lists, rich autoscaling policies (percentage/webhook/schedule/chain), Reserved state and rolling updates, multi-port specs, health/eviction policies, multi-cluster allocation, Grafana dashboards, feature gates. **Ahead:** everything else — Agones/OM deliberately stop at orchestration; musubi adds accounts, social, chat, economy, authoritative matches, meshing, parties/lobbies, rating, and a 9-provider fleet layer that does not require Kubernetes.

**Beamable** — Beamable (266 rows: 99 have / 94 partial / 36 missing / 37 n/a). **Behind:** engine SDKs (Unity/Unreal/Web with codegen), Portal + extensible console, built-in analytics (Athena SQL, KPIs), A/B Trials, C#/.NET microservices with client codegen, in-engine content authoring with promotion, campaigns wizard, scheduled-job management, cloud-save file sync, text moderation provider, enrichment stats. **Ahead:** first-party realtime chat depth, dedicated-server fleets, authoritative WASM matches, relay/transports, server meshing, console logins, passkeys/MFA, marketplace/trading, workflows — Beamable is client-authoritative and SaaS-only.

**brainCloud** — brainCloud (bitHeads) (176 rows: 80 have / 53 partial / 23 missing / 20 n/a). **Behind:** 12 client libs + 6 S2S libs, Portal-X (~110 screens), A/B testing and LiveOps campaigns, segment rule builder, redemption-code campaigns, turn-based async match service, built-in analytics, user/global files with CDN, global statistics grammar, item lifecycle/effects, tournament templates, min-client-version enforcement, dormant-account policy, JS/Lua scripting with editor/MCP. **Ahead:** self-hosting, multi-transport realtime, authoritative matches, meshing, 9-provider fleets, clustering, modern auth (passkeys/MFA/console logins), cross-cell transactions, workflows.

**Colyseus** — Colyseus (OSS + Cloud + Arena) (145 rows: 66 have / 49 partial / 15 missing / 15 n/a). **Behind:** 14 platform SDKs, Predict client library, @colyseus/admin UI and Monitor/Playground, @colyseus/schema field-level codec with delivery modifiers, tutorials/templates, 200+ OAuth providers, published test kit, SQLite dev mode, simulated latency, arbitrary HTTP routes, GeoIP plugin, CRDT sync, per-resource moderator roles. **Ahead:** musubi has the whole player-services layer Colyseus lacks (identity incl. consoles, economy, leaderboards, groups, compliance), plus fleets, meshing, QUIC/WebTransport/UDP transports, clustering and Postgres-backed durability.

**Edgegap** — Edgegap (Arbitrium, Private Fleets, Matchmaker Gen2, Server Browser, Relay, Managed Clusters…) (176 rows: 67 have / 59 partial / 33 missing / 17 n/a). **Behind:** engine SDKs with build→deploy pipelines, dashboard, container log capture and per-server metrics, client server browser with seat reservations, standalone UDP relay for third-party netcode, global ping beacons, per-server resource/TTL/restart specs, deployment webhooks, encrypted secrets, geo-radius placement, named multi-port mappings with TLS upgrade, session objects, metadata-driven scaling policies, MCP server. **Ahead:** Edgegap is orchestration-only; musubi adds the full backend (accounts, social, economy, chat, leaderboards), authoritative matches, meshing, matchmaking with ratings and parties, and self-hosting with 9 providers including Edgegap itself.

**EOS** — Epic Online Services (134 rows: 37 have / 50 partial / 19 missing / 28 n/a). **Behind:** C/C#/Unreal/Unity SDKs, Easy Anti-Cheat, hosted voice/RTC, P2P NAT traversal + relay, Developer Portal, first-class Sanctions with appeals, hosted account portal with OAuth scopes/consent, KWS parental verification, Social Overlay, localized rich-presence templates, per-action client policies, analytics dashboards, typed lobby attributes with search operators, encrypted per-player file storage, crossplay plumbing. **Ahead:** EOS has no economy/inventory/progression/chat-history/groups/matchmaking-with-ratings/dedicated-server orchestration/meshing; musubi is self-hosted with a single identity model and server-authoritative gameplay.

**GameLift** — Amazon GameLift (Servers, FlexMatch, Streams) (277 rows: 59 have / 110 partial / 40 missing / 68 n/a). **Behind:** FlexMatch declarative rule sets with typed attributes and asymmetric teams, managed relay/IP-hiding gateway, native server & client SDKs, game-session log capture, server telemetry + dashboards, async placement objects, metric-threshold autoscaling, Spot capacity, estimated wait time, per-player session limits/protection, console UI, container groups with rolling deploy, IaC/CRDs. **Ahead:** GameLift is hosting+matchmaking only (and AWS-only); musubi offers the complete backend, authoritative matches, meshing, multi-cloud/bare-metal providers (including GameLift as a provider), and no per-instance-hour billing.

**Hathora** — Hathora Cloud (acquired by Fireworks AI; platform frozen, GameFabric transition) (224 rows: 93 have / 55 partial / 30 missing / 46 n/a). **Behind (historical — Hathora is being shut down):** log tail/download, per-server resource time series, SDKs for TS/C#/Go/Java/Godot/Unreal, web console, node-level autoscaling/cloud burst, managed image builds with promote/rollback, enforced resource limits, rooms-per-process packing, egress metering, DDoS posture, fine-grained token scopes, operator teams, server inventory search, static allocation. **Ahead:** every game service (identity, social, economy, chat, leaderboards, matchmaking, meshing, LiveOps) — Hathora hosted containers only.

**LootLocker** — LootLocker (163 rows: 63 have / 54 partial / 35 missing / 11 n/a). **Behind:** SDKs for 7 engines, web console, hosted auth.game flows/Remote Login/Player Portal, rich player segments with session gating, unified Reward + no-code Triggers/campaigns, Stripe checkout and subscriptions, player files with revisions, followers graph, playtests/surveys/signup fields, Discord bot, platform-key campaigns + Twitch Drops, Insights analytics, SDK error reports, CRM connectors and localized broadcasts, classes/heroes loadouts. **Ahead:** self-hosting, realtime (transports, relay, authoritative matches, meshing), matchmaking/ratings, parties/lobbies, groups depth, chat, console logins/passkeys/MFA, workflows, fleets.

**Meshing vendors** — Server-meshing vendors: coherence, Hadean, SpatialOS (archived), Flax netcode, Star Citizen (240 rows: 134 have / 59 partial / 23 missing / 24 n/a). **Behind:** no engine SDKs (coherence no-code Unity layer, SpatialOS GDKs), 2-D only (no octree), no non-spatial/layer partitioning, no network LOD/per-field quantization schema, no visual inspector, no client prediction/interpolation helpers, no client-authority transfer model, single-radius interest, no typed commands, no per-entity replication rate, no memory-pressure admission, no engine-driven simulated players, no >2k-CCU benchmark. **Ahead:** durable world persistence with journal/time-machine, automatic quadtree split/merge, cross-cell atomic transactions, lag-comp rewind, placement strategies incl. WASM hooks, Apache-2.0 with no CCU caps — and the full backend around the world layer (coherence/Hadean have none; SpatialOS is dead).

**Metaplay** — Metaplay (201 rows: 47 have / 73 partial / 56 missing / 25 n/a). **Behind:** full Unity SDK, LiveOps dashboard, native LiveOps events with phases/local time, A/B experiments as config variants, spreadsheet game-config pipeline with diffs, deterministic shared client/server logic, per-player event log, analytics sinks, maintenance mode/developer flag/time-skip, localization pipeline, incident reports, async defender matchmaker, offer placements/groups, composable segments, shipped dashboards. **Ahead:** realtime authoritative matches, synchronous matchmaker, server meshing, dedicated-server fleets, multi-transport, groups/chat depth, console/passkey/MFA auth, Apache-2.0 self-hosting (Metaplay is per-seat licensed).

**Hiro/Satori** — Hiro (game systems on Nakama) + Satori (LiveOps) + Heroic Cloud (154 rows: 33 have / 72 partial / 39 missing / 10 n/a). **Behind:** client SDKs, native remote config/flags/experiments, player analytics pipeline, cohort event leaderboards with tiers, recurring nested achievements, team meta systems (team achievements/gifts/store), player-created challenges, rewarded-video verification, incentives/donations, deeper reward DSL, unlockable slots/energy overfill, operator messaging templates with translations, lifecycle journeys, cross-system preconditions, tutorials system. **Ahead:** everything Hiro needs Nakama Enterprise/Satori SaaS for comes in one Apache-2.0 binary: clustering, meshing, transports, authoritative matches, fleets, workflows, trading/marketplace, bracket tournaments, compliance.

**Nakama** — Nakama server (OSS + Enterprise) + Console + Heroic Cloud (180 rows: 117 have / 39 partial / 9 missing / 15 n/a). **Behind:** 11 maintained client SDKs with session refresh, Nakama Console (browsers, API explorer, roles/MFA), Lua/JS/Go runtimes, generic before/after hooks on every API, console SDKs, more IAP providers (FB Instant, Unity IAP), Streams primitive, matchmaker override/processor hooks, storage index boosting/fuzzy/regex, party label/listing, RegisterHttp, GDPR tombstones, tournament attempts, broader host API, CockroachDB/gRPC. **Ahead:** clustering in OSS (Nakama needs Enterprise), server meshing, QUIC/WebTransport/WebRTC/UDP, deterministic rollback & spectators, fleets, economy depth (store/IAP/subscriptions/market/trading/crafting), ratings, workflows, passkeys/MFA/console logins, compliance tooling.

**Photon** — Photon (Fusion, Quantum, Realtime, PUN, Chat, Voice, Video, Server, Cloud, Tournament SDK) (280 rows: 134 have / 59 partial / 38 missing / 49 n/a). **Behind:** SDKs for every engine incl. consoles, engine-level state replication (Fusion/Quantum) with prediction/interpolation, deterministic predict/rollback with fixed-point/checksums, lag-compensation hitboxes, dashboard, Voice/Video, 16-region hosted cloud with free tier, client-host NAT punch-through, per-object send priority, async/turn-based rooms, event cache for late joiners, Oculus/Viveport/Epic/PlayFab auth, input-delay tuning, AppVersion isolation. **Ahead:** Photon has no player-services layer at all; musubi adds identity, social, economy, leaderboards, matchmaking with ratings, fleets, meshing, compliance — self-hosted without CCU pricing.

**PlayFab** — Azure PlayFab (Core, Multiplayer Servers, Party, Lobby/Matchmaking, Economy v2, Experiments, Insights) (270 rows: 124 have / 64 partial / 56 missing / 26 n/a). **Behind:** Game Manager console, official SDKs/engine plugins, PlayStream event history + Data Explorer/Trends, A/B Experiments, tracked segments with actions and rules engine, client telemetry ingestion, CDN title files, per-entity files/Game Saves, Economy v2 catalog depth (draft/publish, locales, search, reviews), Party client networking + voice, Title News/localized templates, Character entity, export connectors, Facebook Instant/GPGS/Kongregate providers (Facebook itself exists — verdict corrected), rewarded ads/PSP checkout/sweepstakes. **Ahead:** self-hosting/data residency, server meshing, rollback netcode & spectators, QUIC/WebTransport, fleets with 9 providers, groups depth, trading/marketplace, workflows, passkeys/MFA.

**Pragma** — Pragma Engine (227 rows: 118 have / 63 partial / 28 missing / 18 n/a). **Behind:** Game/Social Operator portals + Player Portal, Unity SDK and codegen'd clients, typed end-to-end RPCs with generated stubs, multi-title shared social shard, custom RBAC roles + IdP SSO, data-rights export workflow, telemetry pipeline, login-queue VIP/rate admission, Limited Access Mode, account-linking policies, Steam Family Sharing ban propagation, bundled Grafana/Datadog, Twitch Drops, batch session broadcasts, response compression and encrypted config secrets. **Ahead:** public Apache-2.0 source and self-hosting, economy/IAP depth (Pragma removed inventory, no mobile stores), progression systems, chat, leaderboards/tournaments, meshing, fleets, matchmaker ratings, workflows.

**Rivet** — Rivet (rivet.dev) — actors/workflows orchestrator; game-backend product discontinued with v2.0 (2025-09) (205 rows: 79 have / 65 partial / 23 missing / 38 n/a). **Behind:** SDKs for every runtime, dashboard with live actor inspector, generic durable actor primitive with per-actor SQLite, per-actor queues/alarms/cron, `logs --follow`, docs/MCP/skills, namespaces with PR previews, live WS migration/hibernation, scoped grants/publishable keys, backup/PITR runbook, serverless mode, shipped Grafana/ClickHouse, per-query profiling, PaaS one-click deploys, starter templates. **Ahead:** Rivet (post-v2.0) has zero game-domain services; musubi is far ahead on identity, social, chat, leaderboards, matchmaking, fleets, meshing, economy, LiveOps and compliance.

**SpacetimeDB** — SpacetimeDB (Clockwork Labs) (147 rows: 62 have / 41 partial / 20 missing / 24 n/a). **Behind:** client SDKs with local cache/reconnect, filtered live-query subscriptions with atomic deltas, server-defined views, admin console (table browser, SQL, logs), schema-as-code typed tables with auto migrations, C#/TS/C++ server modules, module HTTP handlers, typed client codegen, lifecycle hooks and per-module logs, outbound HTTP procedures, backup/PITR, typed module env vars, MCP, web-framework hooks, init templates. **Ahead:** horizontal scale-out of one world across nodes (meshing), fleets, Helm/Terraform, Raft HA in OSS, Apache-2.0 vs BSL (1 prod instance), and the complete game-services layer SpacetimeDB leaves to the module author.

**Steamworks** — Steamworks (Valve) backend/Web API features (218 rows: 108 have / 56 partial / 22 missing / 32 n/a). **Behind:** client SDKs/engine plugins, App Admin editors, Steam Datagram Relay-class global relay/DDoS shield, P2P with NAT traversal, server-browser semantics (favourites/history/friends/LAN), achievement sets with localized text/icons/global %, stat constraints and AVGRATE, lobby parity (types, numeric filters, per-member KV, distance), signed inventory proofs, playtime drops and promo rules, OpenID web login/encrypted app tickets/Family Sharing, settlement/chargeback feed, Workshop depth, async Game Notifications, crash/minidump intake. **Ahead:** musubi is platform-neutral and self-hosted: cross-platform identity, economy across stores, chat, groups, matchmaking with ratings, authoritative matches, fleets, meshing, compliance.

**UGS LiveOps** — Unity Gaming Services — LiveOps (Authentication, Cloud Code, Cloud Save, Economy, Leaderboards, Remote Config, Friends, Analytics, Push, Game Overrides, Access Control, Triggers, Scheduler) (205 rows: 111 have / 64 partial / 14 missing / 16 n/a). **Behind:** Unity packages per service + Editor deployment window, Player Management UI, JS/.NET Cloud Code with templates/debugger, Remote Config targeting (JEXL, rollout %, schedules), A/B experiments with significance, Event Manager/Data Explorer analytics, declarative Triggers and runtime schedules, Cloud Save Files (1 GiB), resource-policy Access Control, webshop/D2C payments, Code-Link/Oculus/GPGS auth, push reporting, RateLimit headers and deprecation policy, hosted log query and Secret Manager. **Ahead:** console sign-in, MFA/passkeys/magic links, lockout/CAPTCHA/login queue, session management, account merge, realtime (transports, matches, meshing), fleets, groups/chat depth, trading/marketplace, workflows, self-hosting.

**UGS Multiplayer** — Unity Gaming Services — multiplayer (Sessions, Lobby, Matchmaker, Relay, Distributed Authority, QoS, Vivox, NGO/N4E, Multiplay → Rocket Science) (239 rows: 85 have / 66 partial / 40 missing / 48 n/a). **Behind:** Unity C# Sessions SDK, Vivox voice engine with safety pipeline, Distributed Authority relay, console/dashboards, declarative matchmaking rule DSL with staged relaxations and pools, backfill ticket lifecycle, lobby reconnect/host migration and query engine, incidents/sanction catalog, standalone UDP/DTLS relay with join codes, per-ticket matchmaker logs and stored results, A/B of matchmaking rules, Unity/Unreal server plugins with SQP/A2S and log/crash capture, hybrid multi-location fleets, QoS packet-loss + client SDK. **Ahead:** self-hosted single binary with 9 fleet providers (Multiplay itself was deprecated at Unity 2026-04-01), server meshing, WASM authoritative matches, ready check/dodge/avoid lists/ratings/queue simulation, QUIC/WebTransport/WebRTC/UDP, full player-services layer.

## 2. Unified feature matrix

Rows are normalized features grouped by area. The first block in each area holds features reported by several competitors (deduplicated into one row — the merged source rows are listed in the backlog, §3); the second block ("single-competitor features") keeps every remaining inventory row so nothing is collapsed away. Column order: AccelByte · Agones/OM · Beamable · brainCloud · Colyseus · Edgegap · EOS · GameLift · Hathora · LootLocker · Meshing vendors · Metaplay · Hiro/Satori · Nakama · Photon · PlayFab · Pragma · Rivet · SpacetimeDB · Steamworks · UGS LiveOps · UGS Multiplayer · **musubi**.

### 2.1 auth (30 normalized features, 140 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Email + password accounts (register, verify, reset, lockout, CAPTCHA)** | ✅ ×2 |  | ✅ ×2 | ✅ ×6 | ✅ ×2 | ✅ | ✅ ×3 |  |  | ✅ ×4 |  |  |  | ✅ |  | ✅ ×3 |  | ✅ | ✅ |  | ✅ ×2 |  | ✅ 19/6/2 |
| **Device / anonymous (headless) login** | ✅ ×3 |  | ✅ ×2 |  |  |  | ✅ ×2 |  |  | ✅ ×2 |  | ✅ | ✅ | ✅ | ✅ ×2 | ✅ | ✅ |  |  |  | ✅ |  | ✅ 14/2/0 |
| **Session / token model: JWT, refresh rotation, revocation, JWKS** |  |  |  | ✅ | ✅ ×2 |  | ✅ |  | ✅ | ✅ ×2 |  |  |  | ✅ ×2 | ✅ |  | ✅ ×3 | ✅ | ✅ ×2 |  | ✅ ×4 |  | ✅ 16/3/1 |
| **Generic OIDC / OAuth2 provider catalog** | ✅ ×2 |  |  |  |  |  | ✅ ×2 |  |  | ✅ ×2 |  | ✅ |  |  |  | ✅ ×2 | ✅ ×2 |  | ✅ ×3 | ✅ | ✅ ×2 |  | 🟡 8/6/2 · P0 |
| **Account linking / unlinking / merge policies** | ✅ |  | ✅ |  |  |  |  |  |  |  |  | ✅ ×2 |  | ✅ ×2 | ✅ | ✅ ×6 | ✅ ×4 |  |  |  | ✅ ×2 |  | ✅ 11/5/2 |
| **Apple / Game Center / Sign in with Apple** |  |  | ✅ | ✅ ×2 |  |  |  |  |  | ✅ ×2 |  | ✅ |  | ✅ |  | ✅ | ✅ |  |  |  | ✅ ×2 |  | ✅ 10/1/0 |
| **Xbox Live / GDK (XSTS) login** | ✅ ×3 |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  | ✅ |  | ✅ ×5 | ✅ ×2 |  | ✅ |  | ✅ |  | ✅ 7/4/2 |
| **MFA / 2FA (TOTP, backup codes, email code) incl. admin enforcement** | ✅ ×3 |  | ✅ | ✅ ×2 |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ ×2 |  |  | ✅ | ✅ ×2 |  |  | 🟡 1/7/1 · P0 |
| **RBAC for operators: custom roles composed of permissions, console roles/ACL templates** | ✅ ×2 |  | ✅ |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  | ✅ | ✅ | ✅ |  |  | ✅ |  | 🟡 1/8/0 · P0 |
| **Meta Quest / Oculus, Viveport, other platform IdPs** |  |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  | ✅ ×2 | ✅ | ✅ ×2 |  |  |  | ✅ |  | ❌ 0/0/8 · P0 |
| **Multi-tenant / multi-title / namespace model (publisher + game namespaces, environments)** | ✅ ×4 |  | ✅ ×6 | ✅ |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ ×2 |  | ✅ ×2 |  |  |  |  | 🟡 5/7/3 · P0 |
| **Steam login (session ticket / web API key / OpenID / encrypted app ticket)** |  |  | ✅ ×3 | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ | ✅ |  |  | ✅ 6/2/0 |
| **Custom / server-minted authentication (BYO identity)** |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  | ✅ ×2 |  | ✅ |  |  |  |  | ✅ |  | ✅ 4/1/0 |
| **Google / Google Play Games Services login** |  |  | ✅ ×2 | ✅ |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |  |  |  | ✅ |  | 🟡 3/3/0 · P1 |
| **Hosted login / account portal (web pages for sign-in, reset, link, consent)** |  |  | ✅ |  |  |  | ✅ |  |  | ✅ ×5 |  |  |  |  |  |  | ✅ |  |  |  | ✅ |  | ❌ 1/2/5 · P0 |
| **PlayStation Network login** |  |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  | ✅ | ✅ ×2 | ✅ |  |  |  |  |  | ✅ 4/1/0 |
| **Epic Games (EOS) login** |  |  |  |  |  |  | ✅ ×5 |  |  | ✅ |  |  |  |  | ✅ |  | ✅ ×3 |  |  |  |  |  | 🟡 3/5/1 · P1 |
| **Login queue / admission control / VIP bypass** | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×3 |  | ✅ |  | ✅ |  | ✅ 4/1/1 |
| **Magic link / passwordless email login** |  |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |  | ✅ |  | 🟡 2/2/0 · P1 |
| **Nintendo (NSA) login** |  |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  | ✅ 4/0/0 |
| **Facebook / Instant Games login** |  |  | ✅ | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ 3/0/0 |
| **OAuth2 clients / app registrations (client ids, secrets, redirect URIs)** | ✅ |  |  |  |  |  | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 0/2/1 · P1 — client registrations + client_credentials (P1-23) and authorization-code + PKCE with refresh rotation, player grants (`/v1/oauth/authorize`, `/v1/account/oauth/grants`) |
| **Platform ownership / entitlement check at login (Steam app ownership, Family Sharing)** |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |  |  | ❌ 1/1/1 · P1 |
| **Scoped / down-scoped tokens, publishable keys, per-action client policies** |  |  |  |  | ✅ |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 0/2/1 · P0 |
| **Discord / Twitch login & linking** |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ 2/0/0 |
| **Profile field edit restrictions and input validation rules (username regex, cadence)** | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ 1/1/1 · P1 |
| **Auth security events (suspicious login, critical event history, login history)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Device-code / QR remote login (TV, console, headless)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ❌ 0/0/1 · P2 |
| **Operator SSO (OIDC + SAML) and admin IdP** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Passkeys / WebAuthn** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ 1/0/0 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Account Identifier Tags: admin-defined tags to categorise user accounts (CRUD, query) | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — account metadata/labels via profile custom attributes; no admin tag taxonomy for accounts |
| Country API (country list, per-country age restriction) + Basic country groups (ISO3166-1 groups for pricing/… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no country tables / country groups |
| Custom permissions `CUSTOM:...` for Extend apps in Public Cloud | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — multi-tenant guard; musubi modules run in-process with host-op capabilities |
| Email sending via SendGrid with custom sender domain (CNAME verification by AccelByte ops) | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Account recovery / switch (`RecoverAccountWithEmail`, `PlayerRecoveryOperation.SwitchToAccount`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Facebook data-protection assessment & Apple privacy manifest answers |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Compliance paperwork for SaaS vendor |
| Player-init federation (`IFederatedPlayerInit`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `hook_account_created`/after-auth hooks in WASM can grant items (verify) — no dedicated i… |
| Relay multiplayer (deterministic lockstep) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Advanced auth with extraJson |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — metadata settable after login via `PATCH /v1/account {metadata}`; no registration-time pa… |
| Anonymous (device) auth |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Attach / Detach / Merge identities (per provider) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Handoff / Set-top handoff codes |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `POST /v1/auth/device_code` + `…/token` poll (RFC 8628 shape: pending/slow_down/denied/expired, single-use), approve/deny from a signed-in client, server list/revoke |
| Identity listing & expiry |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/account` lists identities; no token-expiry tracking / refresh of provider tokens |
| Peer apps (shared services between apps) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no peer-app model |
| Player state / profile fields |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `PATCH /v1/account {username, display_name, avatar_url, lang, timezone, location}`; no `i… |
| Session management |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Shared accounts (portal option) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — console feature |
| Summary friend data |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Timed status effects |  |  |  | ✅ on the item-effect table: `PUT /v1/server/effects/{account_id}/{key} {duration_secs|expires_at|permanent, stacks, max_stacks, value, source, mode: replace|extend|stack}`, `GET /v1/server/effects/{account_id}`, `DELETE …/{key}` / `DELETE …` (clear), `GET /v1/server/effects?key=`, client `GET /v1/effects[/{key}]` with `seconds_left`/`stacks`, leader sweep → `effect.expired` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `PUT /v1/server/accounts/{id}/effects/{name} {duration_secs|expires_at|permanent, stacks, max_stacks, details, source, mode: replace|extend|stack}`, server list/clear/search-by-name, client `GET /v1/effects[/{name}]` with `remaining_secs`, leader sweep → `effect.expired` |
| Twitter, Parse, Ultra (blockchain platform) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — niche/legacy providers; musubi covers via `POST /v1/auth/custom` server-verified auth |
| User attributes (key/value tags for segmentation) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `metadata` JSON on account; audiences (`[audiences]`) target by fields, but no attribute… |
| Bring-your-own DB (any ORM/Firebase) from room lifecycle (`onAuth`/`onLeave`) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — modules reach external systems only via `http.fetch` allowlist (no direct DB drivers in W… |
| Client `client.auth.token` sent as `Authorization` header on all HTTP requests, stored in `localStorage`; `on… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no client SDK |
| Exception handling: `onUncaughtException(err, methodName)` wraps all callbacks (since 0.15.55); typed excepti… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WASM traps are isolated per call (fuel/deadline/memory) and never crash the node — strong… |
| HTTP middleware `auth.middleware()` for Express and typed endpoints; verifies signature+expiry only (revocati… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| RelayRoom built-in (broadcast to everyone else, no authority) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `beforeUpgrade(req, context)` transport hook before WS handshake with same read-only context as `onAuth` (`to… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi-edge` per-IP limits and CDN country headers; no user-code pre-upgrade hook (WASM… |
| Per-user authorize / revoke |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Relay session API |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `match_create` relayed match + `match_join`; no IP-based session with webhook |
| Auth status change notifications + expiry notifications (EOS_Connect_AddNotifyAuthExpiration, AddNotifyLoginS… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — JWT exp 15 min + refresh; no SDK callback |
| Connect stores per-identity: provider id, env, account id, last-seen display name (used for leaderboards), ti… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Data access levels: Basic Profile (screen name, purchases), Friends List, Online Presence (basic status vs ga… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — first-party backend owns all data |
| Developer Authentication Tool (local tool to hold dev credentials for repeated logins; EOS_LCT_Developer host… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi: musubi dev + device auth / fixtures (admin fixtures) but no credential-caching de… |
| Exchange-code login from Epic Games Launcher (EOS_LCT_ExchangeCode, AUTH_PASSWORD cmdline) |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Epic store launcher specific |
| Login flow guidance for orgs with multiple products (shared org-level keychain → same PUID mapping per produc… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — multi-project [[projects]] but no cross-project shared identity keychain |
| Login status change notifications (EOS_Auth_AddNotifyLoginStatusChanged) + EOS_Auth_GetLoginStatus |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi: session revoke/kick closes WS (Error/ session close) but no SDK-level status-chan… |
| Logout + EOS_Auth_DeletePersistentAuth |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Application operations |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Service API protocol |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| VPC peering |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted network |
| Authoritative in-process matches via WASM modules, canary/promote/rollback |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Developer tokens: personal vs API |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| GetConnectionInfo (unauthenticated polling) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — connect info reaches the client through `matchmaker_matched.server` / lobby start and gro… |
| Process SSH access for debugging |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted: operator owns the hosts; `[fleet.ssh]` provider manages servers over SSH alr… |
| Product status |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Scope of player identity |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Connected Accounts: link multiple providers to one player (direct in-client or external linking via hosted si… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/account/link/*`, unlink, merge exist (direct linking); no hosted external-linki… |
| Console web UI for Platform Settings (enable providers, enter keys/secrets per platform) |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Providers configured in `musubi.toml` / admin config routes; no admin console UI (README… |
| List connected accounts / disconnect provider (warning when last provider) |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Remember-me sessions (24h default, 30 days with remember_me, configurable length) + check-previous-session API |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Refresh token TTL configurable globally in `[session]`; no per-login remember-me flag |
| Steam session-ticket auth (App ID + Publisher key configured in console) |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Transactional email service config (game name, sender name, reply-to, logo) |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — SMTP/email provider config for magic links exists; no per-game sender/reply-to/logo brand… |
| Realtime authoritative simulation, dedicated servers, server meshing |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Social login conflict resolution: `SocialAuthenticateResult.ConflictingPlayer`, `SocialAuthenticateResolveCon… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — link returns conflict error + `POST /v1/account/merge`; no session-scoped pending-conflic… |
| Default username generator (Player00000001) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi auto-generates usernames on device auth (rg "username" account) but no configurabl… |
| Future-date testing (QA mode): bind identity's new sessions to a simulated date |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — rg -i "fake time\|time travel\|future" -> none (dev fixtures exist, no per-account clock… |
| Identity detail view (events, audiences, experiments, flags, live events, messages tabs); exclude identity fr… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — no console; `GET /v1/admin/accounts/{id}` + audit/events bus only |
| Authoritative matches: runtime match handler (Go/Lua/TS) with `MatchInit/JoinAttempt/Join/Leave/Loop/Terminat… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi authoritative matches run WASM modules (`MatchInput`, `MatchInputAck`, `MatchTickH… |
| Server key gate (`defaultkey`) embedded in client; every client call authorized by server key |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Server-side session cache / `single_session`, `single_party`, `single_socket` config; `sessionDisconnect` run… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Auth data forwarding |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Auth provider "fail if unavailable" |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — provider toggles exist; no fallback-to-anonymous mode |
| Client prediction + resimulation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — server-side rollback params + input buffering + tick hashes (README rollback); client-sid… |
| Connection tokens |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| PlayFab integration |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — playfab mentioned in 6 files (import/migration tooling) but no PlayFab auth provider |
| RPCs |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Room lifecycle webhooks |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Spawn/despawn, prefab table, object pooling, NetworkObject refs, nested objects, scene objects |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — world entities spawn/despawn + handover (`musubi-mesh`); no prefab/pooling (engine-side) |
| Entity tokens: GetEntityToken (title/player/server entity), token lifetime ~24h, ValidateEntityToken (server)… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Login response options (GetPlayerCombinedInfoRequestParams): return player profile, inventory, VC, user data,… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — README login returns account + session + flags; no bundled inventory/statistics payload (… |
| LoginWithAndroidDeviceID / LoginWithIOSDeviceID (device-bound anonymous; OS/device model fields) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| LoginWithServerCustomId / LinkServerCustomId / GetServerCustomIDsFromPlayFabIDs (server-side identity for tru… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| LoginWithSteam (session ticket, TicketIsServiceSpecific), LinkSteamAccount, Server LoginWithSteamId/LinkSteam… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Multiple logins per device: session tickets per title player; GetPlayedTitleList across titles of a publisher |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — Projects are isolated; no publisher roll-up |
| Multiple simultaneous logins per process (instanced API classes, PlayFabAuthenticationContext) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — SDK-side concern; musubi has no client SDK yet (Unreal scaffold only) |
| Player location (country/city/lat-long from IP at login), origination (first identity provider), campaign att… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — Account has `location` field (admin-editable) + country from CDN header for routing; not… |
| Player shared secret + encrypted logins (GetTitlePublicKey RSA, SetPlayerSecret, signed requests, API policy… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No request-signing / per-player secret scheme; musubi relies on TLS + JWT + optional capt… |
| Publisher ID: titles within a studio share a publisher, enabling cross-title master player account + publishe… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — musubi: multi-project config shares one DB/account scope; no explicit publisher-level dat… |
| Connection-token verification by game server |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Discord identity provider |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Login flow: single `LogInV1` returns social+game tokens and login data |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `/v1/auth/*` returns tokens + account; no pluggable "login data" aggregation step (client… |
| Login from a game client (how-to) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Partner (game-server) tokens |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Published scale results |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — README "Measured (not promised)" publishes benchmarks at smaller scale (single node / sma… |
| Session baggage |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ per-session vars seeded at login, `GET/PATCH /v1/session/vars {set, unset}` (fresh access token carries the bag; refresh re-reads it), server `GET/PATCH /v1/server/accounts/{id}/sessions/{sid}/vars`, ≤64 keys / 8 KiB per patch, `session.vars_updated` |
| Session types: Player / Operator / Partner |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Twitch identity provider |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Unsafe identity provider (dev) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Authentication required by default; `auth.insecure_allow_unauthenticated` for trusted networks |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| CORS / origin validation in `onBeforeConnect` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Cloud tokens: `sk_` secret (ns-scoped), `pk_` publishable (browser: resolve/create/connect), `ck_` connection… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — scoped keys and player JWTs exist; no browser-safe publishable key tier (clients always a… |
| In-actor permissions: identify in `createConnState`, gate actions/queues/events, deny-by-default |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Rivet does no sign-in; integrates Auth0, Clerk, Better Auth, Supabase Auth |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Token inspect endpoint `GET /auth/tokens/inspect` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Account linking / merge |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Anonymous / server-issued identity |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Authoritative simulation primitives |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Authorization model |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Column types |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — musubi storage values are JSON; IDL has its own typed fields |
| Identity verify |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Internal-call detection |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Module HTTP handlers (beta) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — modules cannot expose custom HTTP routes; musubi exposes fixed REST + `/v1/rpc` |
| Public key endpoint |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Reducers (transactional RPC) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Rows without PK = set semantics |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Short-lived WebSocket token exchange |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — access/refresh token rotation exists; no dedicated short-lived URL-safe token endpoint |
| Token lifecycle in SDK |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Views (derived, subscribable read models) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — no server-defined derived/subscribable read model; nearest is `StorageSubscribe` on raw c… |
| Account linking guidance (Steam ↔ own accounts) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Authoritative server simulation |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Browser login via OpenID 2.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — musubi has no Steam OpenID relying-party flow; web auth is via its own email/magic-link/p… |
| DLC ownership for connected player |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Encrypted application tickets |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — no encrypted-app-ticket decryption path; `rg -i "EncryptedAppTicket\|encrypted_app"` → 0… |
| Encryption by default |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Free-weekend / timed-trial awareness |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — no license-type metadata in entitlements |
| IP country / server real time |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Message-oriented API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — `match_data_send` targeted/broadcast in relayed matches (≤64 players) covers the same use… |
| Partner-only secure host |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — self-hosted: musubi separates `/v1/server/*` + `/v1/admin/*` (X-Musubi-Server-Key) from p… |
| Player summaries / profile lookup |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Publisher Web API keys |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Session tickets (client → gameserver/backend) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — musubi verifies Steam session tickets server-side (`musubi-integrations` steam.rs calls `… |
| Steamworks partner user management |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — operator SSO (OIDC) + console roles support/liveops/ops/admin (README Operations); coarse… |
| Text filtering of user names/chat |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Account linking rules |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Admin list/get players, change password |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Client API rate limits |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Delete player (client self-delete + admin delete) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Digital Services Act (DSA) notifications to players |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Ban reason/message is returned on login (`ban message`), in-app notifications exist; no D… |
| Disable / enable player (admin) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Events emitted: Signed up, Signed in (consumable by Triggers) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Facebook (incl. Limited Login on iOS) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Player names (auto-generated `Adjective#1234`, set/get, Player Names API) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Player profiles (multiple accounts per device, isolated PlayerPrefs) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Client-SDK concern; musubi has no client SDK yet (server supports any number of device se… |
| Privacy docs per service (Apple privacy manifest, Google Play data safety, retention statements) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Self-hosted: data residency and retention are operator-controlled (`[retention]` config,… |
| Service-account scopes on server sign-in |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Server-key roles `server/admin/support/liveops/ops` are coarse; no per-scope selection at… |
| Steam (session ticket, additional App IDs) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Targeting attributes at authentication time (Targeting Rules → variant tags) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Named audiences (`PUT /v1/server/audiences/{name}`) evaluate rules server-side and are us… |
| Username & password provider |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Netcode for Entities 7.1 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Engine netcode. musubi's server meshing has its own replication model (cells, ghosts with… |
| Topologies |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Dedicated (fleets), relayed client-host, authoritative WASM actor; no DA. |

### 2.2 accounts (4 normalized features, 33 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Account deletion / soft delete / restore / dormant policy** |  |  | ✅ | ✅ |  |  | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/2/1 · P1 |
| **Player search / lookup by id, username, external platform id** | ✅ |  |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/2/0 · P1 |
| **Admin notes / internal comments on accounts** | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Player profile (display name, avatar, metadata, language, timezone, location)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 0/1/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Legacy flagged users |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — migration helper |
| Document history |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Player IDs guidance |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player sessions |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Stream session operations |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Terms of use / setting up |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Egress accounting per process |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg egress` → none |
| Deterministic client system startup order + custom client systems framework (Unity) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — no client framework |
| Nakama compatibility matrix per Hiro version; patch (`_2`) and pre-release (`-pre1`) archives without release… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | — — musubi single binary, semver tags |
| 128-player cap |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Breakpoint/MPPM/Weaver tooling |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Custom player properties |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — match presence metadata & party member metadata exist; no generic per-actor property bag… |
| Data streaming |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| MaxPlayers |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| PlayerTTL |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — match rejoin window exists (session TTL, `ttl` 60 files) but not a per-room configurable… |
| Players lifecycle callbacks |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Samples |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — examples/ + sim-sdk demos; nothing engine-side |
| Server-only session creation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| GetUserPublisherData / Internal / ReadOnly variants (shared across titles of a publisher) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No publisher scope |
| Publisher Data & User Publisher Data (read-only/internal variants) shared across titles in a studio |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No publisher/studio-level data layer across projects |
| Game Instances: multiple simultaneous per player |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Hidden components |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Idempotency key |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Operation permissions |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Transactions / snapshots |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Secure Exec: untrusted JS/TS execution in sidecar VM (contexts, host fns, npm, fs mounts, networking denied b… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Transactions: `c.db.transaction(fn)` named/anonymous, nested disallowed; `c.db.exec/select/get` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — no actor-local DB; `/v1/storage` batch writes are atomic per request (`musubi-storage`) |
| Auto-Cloud (no code) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — client-file-sync model; musubi is a server store (`/v1/storage`), client SDK would implem… |
| Cloud enable/disable by user |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — client preference |
| Custom user id / custom attributes from client |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Session metadata/`client` fields on login feed audiences (platform, lang, location); no a… |
| Game Overrides targeting with JEXL conditions |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Audiences (`PUT /v1/server/audiences/{name}`: lang, location, platform, age bracket, crea… |
| Player-level policies (per-player overrides, list all player policies) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Per-account `limits` (`/v1/admin/accounts/{id}/limits`) and bans/shadow; no per-player ro… |
| Connection approval & max players |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |

### 2.3 storage (12 normalized features, 89 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Per-player files / binary saves (presigned upload, large blobs, revisions, conflict callbacks)** | ✅ ×2 |  | ✅ | ✅ ×2 |  |  | ✅ | ✅ |  | ✅ |  |  |  | ✅ ×3 |  | ✅ ×6 |  |  | ✅ |  |  |  | ✅ 8/4/2 |
| **Key/value player storage with permissions and CAS versions** |  |  | ✅ |  |  |  |  |  |  | ✅ ×3 |  | ✅ |  | ✅ ×3 |  | ✅ |  |  |  |  | ✅ ×2 |  | ✅ 6/4/1 |
| **Backup / PITR / export-import** |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ | ✅ ×2 | ✅ ×3 |  |  |  | ✅ |  |  |  |  | ✅ 5/2/0 |
| **Database backend options (Postgres, CockroachDB, SQLite dev, MongoDB)** |  |  | ✅ ×2 | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×6 |  |  |  |  | ❌ 2/2/4 · P2 |
| **JSON patch / partial update** |  |  |  | ✅ |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ 3/0/0 |
| **Query / index / search over storage (filters, sort, aggregate, full-text, random sample)** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  | ✅ ×2 |  | 🟡 2/2/0 · P1 |
| **Game config / remote config / title data (typed, versioned, per-environment)** |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  | ❌ 0/1/2 · P1 |
| **Offline cache / local-first client data** |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ ×2 |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Title / global files and CDN content delivery** |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ ×3 |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/3 · P1 |
| **Entity / object storage limits and quotas** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ 1/0/0 |
| **Localization pipeline (strings per locale, OTA)** |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Typed collections / schema-as-code / automatic migrations** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 0/1/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Cloud Save upload/download URLs & manifest commit |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Blob presigned S3 routes exist for UGC/storage blobs; no manifest/commit semantics |
| Content tags & tag-based `ContentQuery`; `OmitContentManifestTags` |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No tags on content entries |
| Content validation (`MustReferenceContent`, field validators, Issues badge) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — JSON-schema validation for storage schemas and IDL configs; no reference validators |
| Environments dev→staging→prod & promotion |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `node.environment` + multi-project + content bundle import/export; promotion is manual ex… |
| Old vs new Cloud Save services |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — SDK-side |
| Stats domains/visibility matrix |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Custom Entity S2S admin (collections/indexes) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Global (app) Statistics |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `musubi.global_stats`: `GET /v1/stats/global`, `GET /v1/server/stats/global` (row was stale) |
| Range/number precision note |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Server time |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `GET /v1/time {now, now_ms, iso}` |
| Cloud saves `db.saves.save(userId, data, {slot, expectedVersion})` / `load` — per-user per-slot, monotonic ve… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Config: `maxClients` (auto-lock when full), `patchRate` 50 ms, `autoDispose`, `maxMessagesPerSecond` (exceed… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| GeoIPPlugin (`@colyseus/geoip`) — country from IP at auth via MMDB (BYO file / MaxMind auto-download weekly /… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — country only from CDN header (`cf-ipcountry`/`x-country` → `[regions] country_map`), no M… |
| Inheritance: `.extend()` / `@entity`, collections of polymorphic subtypes registered automatically |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — IDL has no inheritance/union-of-subtypes; JSON state can carry a discriminator field |
| `StateView` per-client visibility (0.16, replaces @filter): `.view(tag)` fields, `view.add/remove/has(instanc… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — matches: `sync_exclude` private paths only (per-player private state) — no per-client arb… |
| `clients` collection (`forEach`, `get(sessionId)`), `client.userData`, `client.auth` (from onAuth), `client.v… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Data types |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| VPC connectivity |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Accounts, profiles, identity linking, MFA |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Applications |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — one musubi deployment = one game; multi-environment by separate clusters/`export/import`;… |
| CreateRoom (admin/dev-token) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Default env vars injected into container |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY` + FleetSpec env (R233, R388); no region/b… |
| GetRoomInfo |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Match recording / replay |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| roomConfig passed to server |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Config history: active/archived/unpublished status, who built/published when; alerts at >100 unpublished / >2… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — content versions table with publish/rollback + prune, no author/status workflow or thresh… |
| Config immutability: read-only type guidance + runtime mutation checker (periodic diff, per-env `GameConfigMu… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — C#-shared-model concept |
| Configs loaded from disk at startup if newer than active (ship config with server build) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi deploy` manifest ships modules/fleets; content via import |
| Sources: Google Sheets (default), CSV, JSON, custom sources; custom `ConfigParser` for types (rewards etc.),… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no spreadsheet/CSV ingestion pipeline; JSON only via `musubi export/import` |
| Collections of JSON objects keyed by (collection, key, owner); batch write in one transaction; read/list/dele… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| List objects: by collection, optional owner, limit (default 10, max 100), cursor paging; public-read-only for… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| `StorageWriteRetry` read-apply-write loop with 0–10 retries on version conflict (runtime) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Custom Authentication (REST) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Input delay (2.1) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 — no input_delay concept (rg=0) |
| Interest management |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| NetworkString / NetworkArray / NetworkDictionary / NetworkLinkedList |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Player protocol |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Qtn DSL unions, bitsets, dynamic collections |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Replays |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| RuntimeConfig / RuntimePlayer / SimulationConfig / SystemsConfig |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| GetTime (server UTC clock for clients) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — No dedicated server-clock endpoint (grep server_time in realtime.rs: none); clients rely… |
| Player Profile (GetPlayerProfile): display name, avatar URL, created/last-login, linked accounts, locations,… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Batch operations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — single-object writes are transactional with versions; a WASM RPC can do multi-collection… |
| Custom config per sub-service (feature toggles) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| DB connection pools |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Dynamic (hot) config reload |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — modules & content bundles hot-reload; core server config is read at start (`rg -i reload`… |
| Encrypted secrets in config |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — secrets via env/K8s secrets + module secret store (`/v1/admin/runtime/modules/{name}/secr… |
| End-game processing hooks |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| HTTP compression |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — WS per-frame compression negotiated (README "Realtime transport: channels and compression… |
| Supported component types |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — musubi stores arbitrary JSON (+ optional IDL schema validation). |
| `FleetServiceConfig` / `ServerPoolManagementPolicyConfig` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| `GameInstanceServiceConfig` knobs |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| `TokenConfig` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Accounts, identity linking, profiles, sessions, MFA, passkeys |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Actor KV driver options for RivetKit standalone: file system, Redis, memory |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Actor state persisted via control-plane KV (reads/writes metered on Cloud) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Connection IDs, `conn.id`, per-connection `conn.state` saved with actor |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Durable in-memory state (`c.state`), auto-persisted, `createState` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Durable workflows `@rivet-dev/workflows`: `ctx.step(name, fn)` records/replays results; steps are the only pl… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Larger-than-memory databases, tiered storage, predictive preload, page cache snapshot API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Postgres driver SSL + client certs; 14 % throughput improvement; no connection poolers supported |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| WS limits: incoming 64 KiB soft / 32 MiB hard, outgoing 1 MiB / 32 MiB, frame 32 MiB, open timeout 15 s, ack… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| `options.actionTimeout` 60 s |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| `options.crashPolicy` restart / sleep / destroy |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — fixed failover behaviour (`musubi-actor`); not configurable per module |
| `options.maxQueueSize` 1,000 / `maxQueueMessageSize` 64 KiB |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — no actor queues |
| `options.noSleep`, `noConnectionSleep` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| `options.stateSaveInterval` 1 s + `connStateSaveInterval` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Auto-increment |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| B-tree indexes |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — Postgres indexes in migrations |
| Default column values |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Direct (dense integer) indexes |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Event tables (transient broadcast rows) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — musubi has typed server→client events (`Notification`, `MatchData`, `PartyData`, `Channel… |
| Keep-alive / liveness |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Primary key / unique constraints |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — fixed schema |
| Public / private tables |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — storage collections have read/write permission levels (`musubi-storage`, `StorageSubscrib… |
| Cloud API (ISteamRemoteStorage) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Cloud Web API (OAuth) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Cross-platform sync control |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — storage is platform-agnostic by default; no per-object platform mask (`rg -i sync_platfor… |
| Developer-only cloud mode |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — dev fixtures/`musubi dev`; feature flags via LiveOps (Zanshin) could gate |
| Network simulation / debug config |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — `rg -i "fake_loss\|packet_loss\|latency_sim"` → 0; `musubi soak` chaos only kills nodes |
| Per-user quota |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Shared cloud between app ids |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — storage is scoped per project (multi-project binary); demo + full game would share a proj… |
| Storage queries / schema |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Access Control configuration deploy from files |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Policies in config/bundles only for storage |
| Client-side caching & offline fallback (RCR cache file) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Client-SDK behaviour (no SDK yet) |
| Environments |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — `node.environment` label + separate `[[projects]]` per deployment (12 source files mentio… |
| JSON settings with schema templates (validation in Editor) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Live Content / Live Releases (config+asset files backed by schemas, release snapshots, release pointers, tags… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Content bundles (`/v1/admin/content`, `/versions`, `/rollback`, import, diff) ≈ releases;… |
| Override priority |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ `remote_config` variants: RFC 7386 merge patches layered on the base document by selector (environment, platform, client version range, country, rollout %), ordered by priority, `GET /v1/server/configs/{name}/preview` shows the resolved result (row was stale) |

### 2.4 social (13 normalized features, 45 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Friends graph (requests, accept, block, lists, notes/favorites)** | ✅ ×2 |  | ✅ ×3 | ✅ ×4 |  |  | ✅ ×5 |  | ✅ | ✅ ×3 |  | ✅ |  | ✅ ×7 | ✅ ×4 | ✅ ×4 | ✅ ×10 |  |  | ✅ ×3 | ✅ ×4 | ✅ ×5 | ✅ 44/7/1 |
| **Notifications / inbox (in-app, TTL, unread, persistent)** |  |  | ✅ | ✅ ×6 |  |  | ✅ |  |  | ✅ ×2 |  | ✅ ×4 | ✅ | ✅ ×2 |  |  | ✅ ×2 |  | ✅ | ✅ | ✅ ×2 |  | ✅ 17/4/2 |
| **Presence / rich presence with localized templates and status** | ✅ ×2 |  | ✅ ×2 |  | ✅ ×6 |  | ✅ ×2 |  |  | ✅ ×3 |  |  |  | ✅ ×4 |  |  | ✅ ×2 |  |  | ✅ | ✅ ×2 | ✅ | ✅ 18/5/1 |
| **Parties (leader, invites, join requests, party data, matchmaking as unit)** | ✅ |  | ✅ ×3 | ✅ |  |  |  |  |  |  |  |  |  | ✅ | ✅ ×2 | ✅ ×7 | ✅ ×6 |  |  |  |  | ✅ | 🟡 9/6/5 · P0 |
| **Announcements / Title News / MOTD / broadcasts (localized, scheduled)** |  |  | ✅ ×6 |  |  |  |  |  |  | ✅ |  | ✅ ×2 |  |  |  | ✅ |  |  |  | ✅ ×3 |  |  | 🟡 3/7/2 · P0 |
| **Push notifications (FCM/APNs/Web Push/HMS, templates, scheduling, local-time, open tracking)** |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  | ✅ ×2 |  |  | ✅ ×3 |  |  |  |  | ✅ ×2 |  | ✅ 5/4/0 |
| **Email delivery / templates / CRM connectors** |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ ×2 | ✅ |  |  |  |  |  | ✅ 3/1/0 |
| **Invites (game/session/party invites, deep links, native platform invite bridging)** | ✅ |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |  | ✅ 4/0/0 |
| **Mail / gifts with attachments and claim** |  |  | ✅ ×2 |  |  |  |  |  | ✅ ×2 |  |  |  | ✅ |  |  |  |  | ✅ ×2 |  |  |  |  | ✅ 3/2/0 |
| **Platform friends import / cross-platform friend matching** |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |  | ✅ 2/0/0 |
| **Followers (one-way follow graph)** |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Referral / invite codes with rewards** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Social overlay / in-game UI layer** |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Social service (99 ops): StatConfiguration (10) incl. import/export CSV, UserStatistic (44: bulk fetch/update… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — stats set/incr/max/min + metadata; cycles/global stats missing; bulk by many users: parti… |
| Connectivity / heartbeat |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WS heartbeat/reconnect protocol exists server-side; no client SDK |
| Friend invite lifecycle & observable lists |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group enrollment types & requirement rules |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group events / group rewards |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group petition / apply / invite / kick / role change |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group recommended & availability (name/tag availability) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Name uniqueness check exists; no recommendations |
| Async turn-based matches |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `/v1/turns` — seats, invites/open/code join, round-robin + explicit next seat, board + version OCC, deadlines with skip|resign, move history, rematch, `turn_match.*` events + no… |
| Group Entities |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group leaderboards |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — leaderboards filter by group members (`http/leaderboards.rs`), group challenges/wars; no… |
| Group monitoring (portal) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — admin group routes exist; no UI |
| Groups (teams/clans) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Shared (friend-readable) user entities |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| User lookup |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Social restrictions surfaced in overlay (social ban days remaining, parental-control / console-settings disab… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi has bans/age-bracket feature closure in API responses; nothing rendered |
| User Info Interface: query by Epic account id, display name, or external account id; display name, nickname (… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| OTA delivery: client auto-downloads published config at session start; connected sessions keep negotiated ver… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — content versions served via `/v1/content/*` + `/v1/configs`; no session-pinned negotiated… |
| Friend codes guide (runtime-generated short codes via storage index + RPC) — pattern, not built-in |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ `GET /v1/friends/code` / rotate / lookup / add-by-code (use- and time-limited) — no friend-code primitive before; would be a WASM mod… |
| Friend metadata (per-edge JSON, runtime-set, 3.26) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Shared Group Data (legacy): small KV blobs shared by ≤~12 players, not scalable, server/client APIs |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Account data reference |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Account search in portal |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Disable incoming friend invites |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ `PUT /v1/friends/settings {requests: anyone|friends_of_friends|code_only|nobody}` — blocklist and appear-offline exist, plus an account-level request policy; was: no "refuse all friend requests" fla… |
| Friend invites by display name or Social ID |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Friend task: SendFriendInviteV1 / AcceptFriendInviteV1 / DeclineFriendInviteV1 / CancelFriendInviteV1 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Identity/friend cache eviction & GDPR note |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — musubi reads from Postgres; retention cleanup job handles PII TTLs. |
| Operator-assigned roles via Account Plugin |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — roles belong to API keys, not accounts. |
| `PartyConfig` knobs |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Docs site with per-page "Copy Markdown", LLM-friendly skill, AsyncAPI/OpenAPI published, Typedoc (retired) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — single long README + `docs/parity.md` + `GET /openapi.json`; no docs site |
| Submodules (TS only) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — musubi modules are separate WASM units with host API; no composable module namespaces/lib… |
| Abuse report |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Clans / groups |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Profile items (equipped) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — Steam profile cosmetics |
| Screenshots / overlay / Remote Play / Steam Input / HTML surface |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — out of scope for a backend |
| Steam Level / badges / owned games / playtime |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — progression tracks/levels and seasons per game (README Progression); no cross-game librar… |
| Steam Timeline / game recording markers |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — client overlay feature (musubi has server-side match recording/replay instead) |
| User reviews API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — store reviews |
| Message to a friend (lightweight payload) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — |
| Profile (name via Player Names) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Relationship refresh / consistency |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Relationship types |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Privacy |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Profile names (Player Names service) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |

### 2.5 groups (4 normalized features, 19 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Groups / guilds / clans (create, join, roles, ranks, permissions, bans)** | ✅ ×3 |  | ✅ | ✅ ×2 |  |  |  | ✅ ×9 |  | ✅ ×2 |  | ✅ ×5 | ✅ ×2 | ✅ ×6 | ✅ ×3 | ✅ ×4 | ✅ ×2 | ✅ ×2 |  | ✅ |  |  | ✅ 24/9/1 |
| **Group chat / mailbox / announcements** |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  | ✅ 2/0/1 |
| **Group treasury / shared inventory / donations / contribution pools** |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  | ❌ 0/1/3 · P1 |
| **Group progression, XP, achievements, challenges, wars** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  | ❌ 0/1/1 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| API rate limits & quotas |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| IAM access control |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — API keys with roles `server/admin/support/liveops/ops`, operator SSO via OIDC, per-projec… |
| Economy: wallets, inventory, store, IAP (Steam/Apple/Google), entitlements, market/order book, fraud heuristi… |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Asset model: Context (grouping, detachable, user-facing, depends-on feature unlock) × Complexity (Generic, Lo… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Items with kind/tags/attributes JSON, unique vs stackable, versions/rollback, convert/mig… |
| Player names auto from platform (Steam, PSN), set/get name, bulk lookup by player ids/platform ids/public UID… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Server-created players from platform ids (e.g. Steam ID) without client call |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Team activity tracking (last active) and membership limits per player |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — group events journal (`GET /v1/groups/{id}/events`); no per-player team-count cap (rg "ma… |
| Team inventory (shared items; admin grant/consume; team-wide limits) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Team leaderboards with per-member contribution tracking |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — README group-owned leaderboard records / team tournaments (0075) and group challenges; pe… |
| Team wallet (shared currencies, admin grant, negative grants, hard currency) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Built-in providers |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — musubi: Steam, Google, Apple, Facebook, Game Center, Xbox XSTS, PSN, Nintendo, OIDC, pass… |
| Event targeting |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| ExpectedUsers slot reservation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Shared config blocks (`shared.configs`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Multi-database per server |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — musubi is one backend deployment per game (worlds/cells shard inside it); no multi-tenant… |
| Cloud Play (GeForce NOW) requirement |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — platform program |
| Conflict resolution |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Custom signaling for P2P |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — WebRTC transport implies musubi signalling for client↔server DataChannels (README Transpo… |
| Server-side access to a player's saves |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |

### 2.6 chat (3 normalized features, 40 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Text chat channels (room/group/direct), history, unread, threads, reactions, edits** | ✅ ×5 |  | ✅ | ✅ ×2 |  |  |  |  | ✅ |  |  | ✅ | ✅ | ✅ ×2 | ✅ ×10 | ✅ ×2 |  | ✅ ×2 | ✅ |  |  | ✅ ×7 | ✅ 20/9/3 |
| **Voice chat (positional, channels, PTT, mute matrix, moderation, recording)** |  |  |  |  |  |  | ✅ ×5 |  |  |  |  |  |  |  | ✅ ×5 | ✅ |  | ✅ |  | ✅ |  | ✅ ×13 | ❌ 2/1/18 · P0 |
| **Chat moderation (filters, mute, slow mode, reports, automod webhook, translation)** |  |  |  | ✅ ×2 | ✅ |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ | 🟡 2/3/0 · P1 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| System transient notifications (categorised, momentary) & system inbox notifications (persistent, draft/send/… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Channels (global, group, dynamic) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Channel types: room (dynamic by name, "millions of users"), group (members only), direct (both must join; inv… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Per-join `persistence` flag (history on/off) and `hidden` presence flag |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Runtime `channelMessageSend/Update/Remove`, `channelMessagesList`, `channelIdBuild`; before/after hooks on ch… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Audio processing API |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| Bandwidth optimisation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| Channel history |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Channel topics / subscriber limits |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Message payloads |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Online status & status message |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Platforms |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| Private 1:1 messages |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Public channels |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Speaker / playback |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| Speaker PlayDelay / jitter buffer |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| Subscriber lists |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| lastMsgId resume |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Contact email: AddOrUpdateContactEmail, RemoveContactEmail, verification state (Pending/Confirmed), ShowConta… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Default languages: title default language + player preferred language (SetProfileLanguage ISO 639-1), used by… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — musubi account `lang` field exists; no title default language or localized content fallba… |
| Party noise suppression (iOS/Android Neon), real-time audio manipulation streams (Windows/Xbox/PS) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Client-side audio DSP; out of scope for a backend (Aurix handles) |
| Photon add-on: PlayFab→Photon auth token, room webhooks (created/joined/left/closed/property/event) to CloudS… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — musubi has native realtime; no Photon bridge needed |
| Access tokens (VAT) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — JWT infra + JWKS exist; voice tokens are delegated to Aurix. |
| Automatic connection recovery |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Blocked-participant filtering in history |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Channel + directed text over TLS |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Channel naming / sharding for >100k PCU |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Couch co-op multi-login |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Echo channels (mic test) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Large 3D channels (≤2000 participants) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Muting matrix |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — Text-chat mute/ban exists (`chat` moderation); no voice. |
| Platform guides: Android (AEC, Bluetooth SCO, proguard, 16 KB pages), iOS (AVAudioSession), macOS, Windows, L… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Platforms |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Player evidence report API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Admin message read + reports. |
| Pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Real-time recording |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| S2S login API (`/api2/viv_signin.php`), fully-qualified identifiers, error handling |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Session groups / multichannel |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Session presence custom metadata (Core 5.28) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Text-to-speech |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |

### 2.7 leaderboards/tournaments (7 normalized features, 27 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Leaderboards (operators, sort, reset schedules, around-owner, friends/social filters)** | ✅ ×5 |  | ✅ ×11 | ✅ ×7 | ✅ ×2 |  | ✅ ×3 |  | ✅ | ✅ ×6 |  | ✅ | ✅ ×8 | ✅ ×12 |  | ✅ ×12 |  | ✅ | ✅ | ✅ ×10 | ✅ ×7 |  | ✅ 67/13/4 |
| **Achievements (incremental, hidden, sets, localized text/icons, global unlock %, recurring)** | ✅ ×3 |  |  | ✅ |  |  | ✅ ×5 |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  | ✅ ×13 |  |  | ✅ 14/7/3 |
| **Tournaments (schedules, entry fees, join codes, prizes, team tournaments, attempts)** |  |  | ✅ ×5 | ✅ ×2 |  |  |  |  |  |  |  | ✅ |  | ✅ ×6 | ✅ ×5 |  |  |  |  |  |  |  | ✅ 15/4/0 |
| **Cohort / event leaderboards with tiers, promotion/demotion, re-rolls** |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ ×3 | ✅ ×3 |  |  |  |  |  |  |  | ✅ |  | 🟡 2/5/0 · P1 |
| **Player statistics (typed stats, scopes, global stats, constraints, AVGRATE)** | ✅ ×4 |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ ×6 |  |  |  | ✅ ×3 |  |  | 🟡 7/6/1 · P1 |
| **Async / turn-based matches (turns, deadlines, versioned state, push on turn)** |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 1/0/0 |
| **Leaderboard metadata / versionId guard / archived versions / rank disable for huge boards** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ❌ 0/0/1 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Bulk stat update endpoints (multi-user, multi-stat), update strategies OVERRIDE/INCREMENT/MAX/MIN, reset user… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — set/incr/max/min via `/v1/stats`; no CSV import/export of stat configs |
| Events (one-off/recurring, phased) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Live events + tournaments + leaderboards cover it, but no multi-phase event entity with p… |
| Events calendar view in Portal (recurring events, tag filter) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No UI |
| Stats processing is asynchronous ("might take some minutes"); client cache persists until SDK shutdown |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi synchronous |
| Config: key, asc/desc, Enable Game API writes toggle, Overwrite Score on Submit (else lower score discarded),… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Score operators + max_num_score + metadata per score |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Time-based vs score-based events |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — no race-to-target cohort completion or multi-round rolls in musubi leaderboards/tournamen… |
| Group/guild-owned records possible via custom owner id (no built-in team semantics) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Record: owner (user, group or custom id), one record per owner, score + subscore, JSON metadata, username cap… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Aggregated (global) stats |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — `GET /v1/server/stats/top` ranks players per stat; no automatic daily global totals histo… |
| Current player count |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Display type |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — pure backend; no Valve-hosted leaderboard page to render into (client SDK concern) |
| Download modes |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Friends-only reads |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — `?friends=1` filter exists, but no per-leaderboard setting that *forbids* global reads to… |
| Offline cache / deferred commit |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — client-SDK behaviour; musubi has no client SDK yet (docs/parity.md) |
| Other users' stats |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Publisher-key only mutations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Score type and details payload |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Sort order |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Upload methods |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Authoring: `.lb` assets in Editor, CLI deploy, dashboard |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — `musubi deploy` manifest; no editor asset type |
| Player-centered range |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — |
| Score metadata |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Scores by player ids (friends-style views built client-side) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Tiers |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Update strategies |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |

### 2.8 matchmaking (27 normalized features, 132 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Matchmaking queues with skill/ping/role/team rules and expansion over time** | ✅ | ✅ | ✅ ×3 | ✅ | ✅ | ✅ ×8 | ✅ ×3 | ✅ ×6 | ✅ |  |  | ✅ |  | ✅ |  | ✅ ×5 |  |  |  | ✅ ×5 |  | ✅ ×3 | ✅ 23/14/2 |
| **Matchmaking security / ops (TLS between services, dashboard protections, error codes, behaviour under load, inspect/debug API)** |  | ✅ ×2 |  | ✅ | ✅ | ✅ ×3 | ✅ | ✅ ×3 | ✅ |  |  |  |  |  | ✅ ×4 |  | ✅ |  |  |  | ✅ | ✅ | ✅ 12/5/0 |
| **Placement: latency/region/geo selection, ping beacons, QoS measurement, multi-region fallback** | ✅ |  |  | ✅ ×2 | ✅ | ✅ ×5 |  | ✅ ×9 |  |  |  | ✅ |  |  | ✅ | ✅ ×2 |  | ✅ |  |  |  | ✅ ×3 | 🟡 11/12/3 · P0 |
| **Backfill (tickets for in-progress matches, roster sync, approval, auto-backfill)** | ✅ ×2 | ✅ ×5 |  |  |  | ✅ ×2 |  | ✅ ×7 | ✅ |  |  |  |  |  |  | ✅ ×2 | ✅ ×3 | ✅ |  |  |  | ✅ ×4 | ✅ 14/11/1 |
| **Lobby operations detail (types, passcodes, joinable flag, data for non-members, per-member metadata, version counter, host reassignment, events push, limits)** |  |  | ✅ ×4 |  |  |  | ✅ ×3 |  | ✅ ×3 |  |  |  |  |  | ✅ ×2 | ✅ |  | ✅ ×4 | ✅ | ✅ ×3 |  | ✅ ×10 | ✅ 24/5/2 |
| **Lobbies (create/join/leave, host, ready, teams, kick, heartbeat, host migration)** | ✅ |  | ✅ | ✅ |  |  | ✅ ×4 |  |  |  |  |  |  |  | ✅ ×2 | ✅ |  |  |  | ✅ |  | ✅ ×10 | ✅ 11/5/4 |
| **Matchmaking tickets lifecycle, status polling, estimated wait time** | ✅ | ✅ | ✅ |  |  | ✅ ×2 |  | ✅ ×2 |  |  |  | ✅ |  |  |  | ✅ ×2 |  |  |  |  |  | ✅ ×3 | ✅ 10/1/2 |
| **Pools / profiles (ordered filtered subsets within a queue, hosting target per pool, large matches, team definitions)** |  | ✅ ×5 |  | ✅ |  | ✅ | ✅ | ✅ ×2 |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ ×4 | 🟡 6/6/2 · P0 |
| **Ticket API (create/cancel/get/delete, watch assignments, service-account tickets, search fields/tags)** | ✅ | ✅ ×10 |  |  |  |  |  | ✅ ×3 |  |  |  |  |  |  |  |  | ✅ ×2 |  |  | ✅ |  | ✅ ×5 | ✅ 17/4/1 |
| **Lobby attributes & search (typed public/private attrs, numeric ops, sort, distance, per-member data)** | ✅ ×2 |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ ×2 |  | ✅ | 🟡 5/5/0 · P0 |
| **Declarative rule DSL / rule sets (comparison, distance, collection, compound, sort rules; typed attributes)** | ✅ ×3 |  |  |  |  | ✅ |  | ✅ ×12 |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 5/9/2 · P0 |
| **Game session CRUD (create/describe/search/update/terminate, properties & data, idempotency)** | ✅ ×3 |  |  |  |  | ✅ ×2 |  | ✅ ×7 |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 6/5/1 · P1 |
| **Pluggable match function (external gRPC/HTTP/WASM MMF, matchmaker override/processor hooks)** | ✅ | ✅ ×14 |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 6/5/3 · P1 |
| **Server browser (list DS/matches with filters, favourites, history, slots, reputation)** |  |  |  |  |  | ✅ ×3 |  |  |  |  |  |  |  |  | ✅ ×4 | ✅ |  |  |  | ✅ ×2 |  |  | 🟡 3/4/3 · P1 |
| **Ready check / accept phase / dodge penalties** |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ | ✅ 3/0/0 |
| **Session objects (game session lifecycle, player sessions, reservations, seats, TTL)** |  |  |  |  | ✅ ×3 | ✅ |  | ✅ ×5 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 4/4/0 · P1 |
| **Matchmaking events & status (searching/potential/accepted/succeeded/timed out/cancelled, connection info)** |  |  |  |  |  | ✅ |  | ✅ ×8 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 5/3/1 |
| **Matchmaking observability (per-ticket X-Ray/log, rule evaluation metrics, stored results)** |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 0/2/0 · P0 |
| **Offline / rating-based async matchmaking (defender selection)** |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 0/2/0 · P1 |
| **Player-initiated private/custom matches and invitation codes** |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 1/1/0 · P1 |
| **Rating systems (Elo, Glicko-2, TrueSkill/OpenSkill, seasons, decay, placement games)** |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ 2/0/0 |
| **Async placement object / placement events / queue of placements** |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/2/0 · P2 |
| **Avoid lists / no-repeat opponents / blocked players exclusion** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ 1/0/0 |
| **Crossplay groups / platform pools / client version isolation** |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **First-party session sync (Xbox MPSD, PSN sessions, Steam lobbies) and platform presence** | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 1/0/0 |
| **Matchmaker controls: pause queue, rebalance, exclusion lists, kill switch, A/B rule variants** | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Matchmaking simulation / load testing of queues** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 0/1/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Cross-play control: match options with any/all/unique criteria on platform list; default crossplay on; platfo… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Join-in-progress opt-out (`new_session_only` ticket attribute), session exclusion (exclude specific sessions,… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — backfill can be disabled per queue; no per-ticket `new_session_only` flag or excluded-ses… |
| Lobby text protocol: `type:` key-value request/response/notif with id correlation; v1 party/matchmaking/frien… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player validation: session secret → TOTP presented by client, DS validates via SDK; secret re-fetch endpoint | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| GKE manifests with Workload Identity (metric writer role) and Cloud Run `service.yaml`/`cloudbuild.yaml` (Mem… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Out-of-cluster access by switching Services to `LoadBalancer`; in-cluster DNS names `om-frontend.open-match.s… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Presentations: Google Doodle (GDC 19), Unity Berlin 2018; Unity co-founded the project |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Redis state store: bundled Bitnami chart with HA option, or external (Memorystore, Redis Enterprise tutorial) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Postgres + in-memory replicated ticket pool (README:matchmaker) |
| Release blog per version with usage requirements (k8s ≥ 1.21, Go ≥ 1.18 at 1.7) and RC cadence |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Scale-test tooling (`frontend scale testing` helm template, e2e tests in `open-match.dev/open-match/testing`) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Single `om-core` binary serving gRPC + grpc-gateway REST (`OM_GRPC_PORT`), 12-factor env config (`OM_*`), CNC… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| gRPC keepalive params, headless Services + DNS `round_robin` client LB, resource sizing |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — HTTP/WS behind a normal LB |
| Matchmaking Monitor & Server Monitor (portal) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/server/matchmaker/stats` (p50/p95/p99 wait per queue×region), Prometheus metrics… |
| LobbyRoom: live listing via `enableRealtimeListing()`; auto updates on create/join/leave/dispose; filter must… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Matchmaker stats API: `fetchAll()` per process `{processId, roomCount, ccu}`, `getGlobalCCU`, `getLocalCCU`,… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| QueueRoom (0.18): rank-based grouping (`compare`), teams (`maxTeamSize`, `teamId`), `maxPlayers` 4, `maxWaiti… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Room-type options `filterBy([...])` (stored client options filter later joins; `maxClients` from room instanc… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/matches?label&min_size&max_size&open_slots&region` label filtering + lobbies fil… |
| Server-side matchMaker API: `createRoom`, `joinOrCreate`, `reserveSeatFor(room, options, authData)`, `join`,… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — server key: `POST /v1/server/matches`, `GET /v1/server/matches/{id}`, `DELETE`, `/label`,… |
| Standalone matchmaker process (`isStandaloneMatchMaker`): shared Presence+Driver, skips IPC/health registrati… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Visibility flags `locked` (auto at maxClients, sticky via `lock()`/`unlock()`), `private` (joinById only), `u… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `roomId` 9-char random, customizable in `onCreate` (recipes/custom-room-id); `roomName`; `metadata` set in on… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Auto-assigned reservation |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/server/allocate {backfill, wait_secs}` is server-key; players go through matchm… |
| Automated ping in SDKs |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No client SDK does this (client must send `ping_<region>` itself) |
| Bulk reservation confirmation by server |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/gameserver/players` updates counts; no reservation reconciliation |
| CORS allowlist for WebGL clients |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `[http] cors_origins = ["https://play.example.com", "*.example.com"]` + `cors_max_age_secs` — exact / wildcard-subdomain / `*` allowlist, preflight for all API methods, Bearer + musubi headers allowed, trace id exposed; empty = no CORS (was: none in server/edge) |
| Config: semver `version`, `inspect` mode, `max_deployment_retry_count`, `allowed_cors_origins`, `ticket_expir… |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `QueueConfig` has wait/widening/ready-check/teams/MMR/ping; no ticket expiration/removal… |
| Deployment selectors (tag-based session routing) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Allocation routed by `pool` + `region` only |
| Federated identity for players |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Georouted allocation with client-measured latency |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group Up (built-in party/lobby → group) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Managed no-code matchmaker (Gen2, v3.3.x) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| No ready-check by design |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Per-instance private API + Swagger UI + auth token |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Rule: `intersection` |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Wildcard/`-field` queries; no set-intersection operator producing a resolved value |
| Rule: `number_difference` |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Rule: `string_equality` |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Search & browse with filter/sort language |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Fixed query params on `GET /v1/matches`/`/v1/lobbies`; no expression language or custom s… |
| Server Token vs Client Token |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Server-to-server proxy pattern |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Session bulk stop |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ `POST /v1/server/sessions/stop {ids | search, reason, dry_run}` — ≤500 per call, same filters as session search, `missing` reported |
| Best display name resolution (online platform name vs Epic display name; EOS_UserInfo_CopyBestDisplayName / E… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi profile has display_name + linked provider ids; no per-platform display-name selec… |
| Mirror session management on remote (non-owner) clients (local copy of session state) |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no client SDK state mirror; WS MatchState sync exists for authoritative matches |
| Permission levels: PublicAdvertised (searchable), JoinViaPresence (unlisted, friends join via overlay/presenc… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi public/private(+6-letter code, password); no presence-join / unlisted-but-joinable… |
| Redistributable installer + Bootstrapper for EOS local service (required for Overlay/Account Portal outside E… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no client-side service needed |
| Sanction enforcement on sessions (RESTRICT_GAME_ACCESS / RESTRICT_MATCHMAKING): sanctioned players cannot joi… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Attribute value |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — string/number props only |
| Destination order & FleetIQ priority |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — region choice by ticket ping props; no cost/destination priority |
| Explicit sort example |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| FlexMatch CloudWatch metrics |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/server/matchmaker/stats` (wait p50/p95/p99, rejections) + Prometheus; no per-rul… |
| Matchmaker locations |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Multi-Region queue / failover |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Party aggregation in rules |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — party tickets kept together; MMR averaged? (`Teams` balancing by MMR) – no custom aggrega… |
| Player attributes |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — ticket `props` (string/number) used by query and `MmrRule.property`; no list/map typed at… |
| Player object in requests |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — ticket `props` flat map; team assigned on match |
| Property expression language |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg -i 'stddev' or 'set_intersection' crates/musubi-matchmaker` → none |
| Queue notifications |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — global webhooks/event bus, not per-queue targets |
| Queue scope & examples |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Regions |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Release notes (shared) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| SNS topic setup / SSE / Lambda subscription |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — webhooks are HTTP |
| Sample matchmaking request |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Lobby → room creation in chosen region |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Lookup by external match id |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Lookup by roomId |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Server-only config updates after start |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Server-side session rules: single active session per player (second device terminates first), `Session:Maximu… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Synchronous (realtime) matchmaker |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| GameLift integration guide (Heroic plugin; requires Nakama Enterprise image or Heroic Cloud) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Guide: exclude blocked users from matchmaking via before-hook query rewrite |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Headless server auth guide (server-to-server via http key + RPC) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Monolithic stateful Go server; subsystems: authorization (JWT), in-memory data (Bluge full-text index for mat… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| No first-class lobby API (built with authoritative match + label, see lobby guide) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Party matchmaking: leader submits ticket with party id, members kept together; parties match with parties and… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Socket-only matchmaking: ticket with Bleve query (string/numeric props, MUST `+`, MUST NOT `-`, SHOULD, range… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| `FleetManager` interface (3.21): `Get/List/Create/Join/Update/Delete` instances, `RegisterFleetManager`, name… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| i3D.net / PurrNet integration guides |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — no i3D-specific provider (webhook/SSH generic provider instead) |
| App & lobby stats |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| GetCustomRoomList |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Matchmaking modes |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| NameServer → Master → Game Server hop |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Server-side skill/role/team matchmaker |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Session API |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Steam auth |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Legacy Matchmaker (Matchmake API with legacy game servers, 1.0 servers) — deprecated/retired |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Legacy |
| Lobby & Matchmaking client SDK (C/C++ PlayFabMultiplayer, Unity plugin, Unreal via OSS): async state-change p… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No client SDK (README: SDKs not written; only IDL-generated stubs via `musubi idl`) |
| Match-based how-to (end-to-end) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Matchmaking metrics / observability |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Matchmaking: Kotlin `MatchmakingPlugin` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Matchmaking: `ExtMatchmakingKey` queue partition + `ExtMatchmakingParty/Player` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Party plugin hooks |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — WASM hooks on party events exist (`party.*` hooks) but fewer interception points (no per-… |
| `LoginQueueServiceConfig` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — capacity + poll interval only. |
| Backpressure: send fails when queue full |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Inspector queue view/push/clear (`/inspector/queue`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Request/response with `wait` and `timedOut` status; `enqueueAndWait` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Token types: namespace public/dev, player, lobby, service/game tokens (protobuf-JWT with label) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| `canPublish` per-queue authorization |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — no queues |
| `run` handler (background loop, does not block startup) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Lobby → game server handoff |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Max members |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| PSN boot invite check |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — console platform plumbing |
| Party beacons (ISteamParties) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — parties exist (`party_create/join/promote/data_send…`, join requests) but no public "beac… |
| Per-member metadata |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — member `team` and `ready` fields only (`…/team`, `…/ready`); no arbitrary per-member KV (… |
| Rich-presence join |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — rich status JSON ≤2 KB can carry a connect payload, but there is no client to render "Joi… |
| Access control policies |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Operator roles (server/admin/support/liveops/ops), server keys and per-key quotas; no dec… |
| Bulk update (service account only) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — Server key can call individual routes; no transactional bulk lobby mutation. |
| Cloud-mediated client-authoritative sessions (CMB) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — No client-authority object-ownership relay; musubi offers server-authoritative WASM match… |
| Config-as-code deployment |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Content bundles (`GET/POST /v1/admin/content`, dry_run, rollback) cover economy/rating qu… |
| Create lobby |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Custom `INetworkHandler` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — SDK extension point. |
| Custom session properties with visibility + indexing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Lobby metadata is one JSON blob ≤16 KB (bigger) but no per-property visibility and no ind… |
| Delayed vs automatic network start |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Direct network options |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| External data in rules (server-authoritative skill) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Server-side rating (Elo/Glicko-2/OpenSkill, seasons) is pulled into tickets automatically… |
| Heartbeat / inactivity cleanup |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Integrations with Leaderboards / Cloud Save / Cloud Code / Analytics |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Lobby `version` counter |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Storage objects have CAS versions; lobby object has `updated_at` but no monotonic version… |
| Lobby bandwidth pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Self-hosted. |
| Lobby → voice/notification tokens |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — `POST /v1/voice/token {party_id / match_id}` (Aurix) — not for lobbies; lobby chat room e… |
| Local dev server allocator |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Matchmaker statistics per queue/region |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Netcode for Entities integration |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — No N4E driver. |
| Privacy manifests / data safety |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Self-hosted (developer is controller); musubi has GDPR export/delete routes. |
| Queues (mutually exclusive ticket sets) + default queue fallback |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Rule samples library |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — README examples for queue config; no sample library. |
| Session Observability API (deprecated) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Match journals / recordings + audit log, not a matchmaking timeline per session. |
| Session viewer / view sessions (dashboard) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — No admin console (README: "admin console not written"); `GET /v1/admin/*` lobby listing o… |
| Simulation / offline evaluation of rules |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Troubleshooting guide / error reasons |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Tutorials: Relay + NGO, build your first session |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Verbose logging |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Client-side. |
| Wire push SDK dependency |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Single realtime socket. |
| `SessionObserver` per session type |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — SDK concern. |

### 2.9 realtime/matches (18 normalized features, 132 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Realtime transports (WebSocket, QUIC, WebTransport, WebRTC DC, UDP) and channel classes** |  |  |  | ✅ | ✅ ×3 |  |  | ✅ ×3 | ✅ |  | ✅ ×5 |  |  | ✅ | ✅ ×9 |  |  | ✅ ×4 | ✅ ×2 | ✅ |  | ✅ ×4 | ✅ 16/9/3 |
| **Relayed (client-hosted) rooms/matches with host migration** | ✅ ×2 |  |  |  |  | ✅ |  |  | ✅ |  | ✅ ×2 |  |  |  | ✅ |  |  |  |  | ✅ ×2 |  | ✅ | 🟡 4/5/1 · P0 |
| **Deterministic lockstep / rollback netcode (fixed-point, input relay, checksums, desync detection, replays)** |  |  | ✅ |  |  |  |  |  |  |  | ✅ | ✅ ×4 |  |  | ✅ ×4 |  |  | ✅ ×4 | ✅ ×4 |  |  |  | ✅ 8/2/3 |
| **Reconnect / resumable sessions / WS migration & hibernation** | ✅ |  |  |  | ✅ ×4 |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ | ✅ ×3 |  |  |  | ✅ ×2 | 🟡 6/4/2 · P0 |
| **Client prediction / reconciliation / interpolation library** | ✅ |  |  |  | ✅ |  |  |  |  |  | ✅ ×3 |  |  |  | ✅ ×8 |  |  |  |  |  |  | ✅ | ❌ 1/3/4 · P0 |
| **State sync / schema codec (field-level change tracking, per-field delivery modifiers, quantization, callbacks)** |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  | ✅ ×2 | ✅ | ✅ |  |  |  |  | ✅ | 🟡 2/4/0 · P1 |
| **Match RPC / messaging (targeted, broadcast, reliable/unreliable, priorities, interest groups)** |  |  |  |  | ✅ |  |  |  |  |  | ✅ ×3 |  |  |  | ✅ ×7 |  |  | ✅ |  |  |  |  | 🟡 6/4/2 · P1 |
| **Standalone UDP/DTLS relay for third-party netcode (Mirror/FishNet/NGO/UTP/Unreal transports) with join codes** | ✅ |  | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×2 | 🟡 0/2/1 · P0 |
| **Workflows / durable actors / timers / alarms / cron / mailboxes** |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |  |  |  |  | ✅ 2/1/0 |
| **Generic streams primitive for module code (join/leave/send/count on arbitrary streams)** |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P1 |
| **Lag compensation / rewind queries (hitbox history)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ | ❌ 1/0/1 · P1 |
| **Match / room lifecycle webhooks & server-to-server callbacks (create/join/leave/close/custom events)** |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/1/0 · P1 |
| **P2P / NAT traversal (ICE/STUN/TURN) with relay fallback** |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  | ✅ |  |  | ❌ 1/1/1 · P0 |
| **Simulated latency / network condition testing** |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ 0/1/1 · P1 |
| **CRDT / collaborative document sync** |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — 0/0/0 |
| **Event cache / late-joiner state replay** |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  |  | ✅ 2/0/1 |
| **Room/match limits (players, message size, rate, rooms per process, CCU per node)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ 1/0/0 |
| **Spectators / replays / match recording** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  | 🟡 0/2/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| One-way offline matches (CoC-style) + Playback Streams |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — replay recording of authoritative matches (`musubi-actor` replay, README) covers playback… |
| Game loop: `setTimestep(cb, delay)` variable step (default 16.6 ms) and `setFixedTimestep(step, tickRate)` fi… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Message composability: `Messages<R>` typed handler maps spread into `messages`, later overrides earlier |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — module code organization is the author's choice (Rust/AssemblyScript/etc. compiled to WAS… |
| Patch cost model documented (1 B field header, refId sizes, varint number sizes, `sort()` resends all), chang… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — postcard binary + lz4 and RFC 6902 ops; no author-side tracking control (diff is computed… |
| Primitive types: string (≤4 GiB), number (varint auto), boolean, int8/16/32/64, uint8/16/32/64, float32/64, b… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — IDL scalar types (string/int/float/bool/bytes…) — no explicit 8/16-bit wire widths, postc… |
| Room Plugins (`definePlugins`, `RoomPlugin` base): contribute messages, lifecycle hooks (before/after order p… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — before/after hooks exist at server level (`hook_*` module exports, `before_`/`after_` for… |
| Timing: `clock.setTimeout/setInterval/clear/start/stop/tick`, `elapsedTime/currentTime/deltaTime`; `Delayed`… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| UniqueSessionPlugin — max N concurrent sessions per user per room type, `onDuplicate reject\|replace`, `rejec… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Relay session status in dashboard |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/server/matches` lists matches; no UI |
| CloudTrail audit |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Compliance / resilience / vulnerability pages |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Errors |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Pricing page (dev guide) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Stream URL operations |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Stream classes |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Troubleshooting |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Custom roomId |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — match ids are server-generated UUIDs; lobbies have join codes; no caller-supplied match i… |
| DestroyRoom |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| List active / inactive rooms for a process |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Process status enum |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Room status model |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| StopProcess |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| SuspendRoom / ResumeRoom |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — match snapshots + resumable sessions + replay (R208, migrations match_snapshots) restore… |
| Byte budget per client *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client Connections & client messages *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client auth to RS *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client count *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — edge ingress nodes with interest fan-out exist (`musubi-edge`), but no published >10k sin… |
| Client-as-host *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client-server only, ≤100 players *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Cloud Storage *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| CoherenceInput *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `world_input{input: json}` untyped; `match_input` tick-stamped for rollback matches |
| Crash recovery *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Entity-component-worker model, layers *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — single simulator kind per world; no component layers simulated by different worker types |
| Floating origin / 64-bit positions *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — positions are f64 (`WorldEntity.x,y`) but 2-D only; no client origin-shift helper |
| Global Query / Is Global *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no "always relevant" flag; everything is radius-bound (`rg -i global` in mesh: none) |
| Hybrid Service *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — world host node acts as hub (307 redirects, event bus); not a dedicated replication servi… |
| Kick with payload *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Launcher / share links *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no build distribution |
| Max clients per RS *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Max queries per client *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — one `world_subscribe` radius per client; no multi-query count/limit concept |
| Network Bind Culling *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Online Services (gateway matchmaking) *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Persistence (session) *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Protocol version check *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Receive-rate throttling / kick *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Replication Layer separate from game servers *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi keeps authoritative state inside cell simulators with standby replicas and journal… |
| Replication Server (RS) hub *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Roles & ownership *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — server-authoritative only; no client-owned replicated objects |
| Scenes *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Secrets guidance *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Send / receive frequency *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — world tick 30 Hz, per-client `max_hz` on subscribe (`WorldSubscribe.max_hz`); no server→c… |
| Serialized Variable Culling *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Shutdown notice *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Simulation frame / network time *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — tick numbers in `WorldDelta.tick`, `time_scale` (dilation); no client clock-sync/network-… |
| Supported sync types *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| System entities *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Worker heartbeats & window sizes *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — session heartbeat; no per-direction window-size knobs |
| World commands *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Match engine queues: `match.input_queue_size` 128, `call_queue_size` 128, `signal_queue_size` 10, `join_attem… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — actor runtime has tick/mailbox configs but not these exact knobs |
| Single-node match capacity: "hundreds/thousands of matches per node" — matches are not distributed in OSS (no… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| 500 KB per-client server buffer |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Backgrounding behaviour |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — session resume/reconnect exists (`rejoin`/`reconnect` in realtime.rs) but no client-side… |
| Binary protocol docs |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Camera selection, preview rotation, custom shaders |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| Compression |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Custom type serialization |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Dummy traffic keepalive |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| EmptyRoomTTL |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — matches end when empty via actor idle timeout; not per-room configurable by client |
| Event code space |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Eventual (full) consistency model |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| FSM addon |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Fair-use message rate |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Fusion Core: string heap, ObjectChild sub-objects, notify API |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Fusion Statistics 2.1 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — server metrics (Prometheus, match tick metrics); no client overlay |
| HideRoomAfterStartSec / CloseRoomAfterStartSec |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| IPv6 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Instantiation (PUN) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — world entity spawn API server-side; no client view IDs |
| Local Photon Server for plugin testing |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| MasterClientId as room property |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — relay host designated by server; no client-set master |
| NetworkMecanimAnimator |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — — engine-side |
| Networked input |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Nickname |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| OpGetProperties |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Photon Stats GUI / Lag simulation GUI |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — no client overlay; server metrics + soak impairment |
| PhotonTransformView / RigidbodyView / AnimatorView |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — — engine-side |
| Property CAS |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Quantum WebGL & multi-client/MPPM |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Recommended payload <1 KB, max 100 KB |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Relaxed skill matching |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Reliable ordered + fragmentation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Room create/join ops |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Room plugin selection |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| RpcInvokeInfo |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Server timestamp |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Server-side simulation percentage |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Shared Mode Custom Plugins (2.1) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Signals & events |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Start online game from local snapshot (3.0.5+) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — authoritative match can start from stored snapshot (journal/snapshots); no client-sim pro… |
| SuppressRoomEvents / SuppressPlayerInfo (0x40) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 — presence events always sent in relay matches; no suppress flag |
| Traffic stats / support logger / disconnect analysis |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — RTT/ping messages + Prometheus server metrics; no client-side traffic stats SDK (no clien… |
| Unreliable command cap |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Unreliable sequenced commands |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Verified vs predicted frames |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 |
| WebGL tabs in background |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — — client concern |
| Game-server providers (hosting) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `musubi-fleet` providers: local, Docker, Kubernetes/Agones, Firecracker, SSH bare-metal,… |
| Actor keys (string / compound array keys, injection-safe) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — matches/worlds addressed by UUID (`match_create`, `world_subscribe`), modules by name; no… |
| Actor-to-actor calls (`c.client()`), traced end-to-end |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — modules can call host ops (`musubi.host_call`: storage, wallet, notify…) and `match.*` ho… |
| Display name & icon in dashboard |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — no dashboard yet; cosmetic |
| Ephemeral vars (`c.vars` / `createVars`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Input parameters on create |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Long-running workers connect outbound (`registry.startEnvoy()`), no public URL / inbound firewall rule |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — dedicated game servers register inbound with `X-Musubi-Server-Id` and the SDK heartbeat (… |
| Low-level KV (`c.kv` put/get/list-by-prefix/delete, text/binary) — deprecated |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Orchestration error codes: `crashed`, `no_capacity`, `runner_no_response`, `runner_connection_lost`, `runner_… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — fleet allocation errors (`no capacity`, buffer exhausted) in `fleets.rs`; less granular |
| Path-based routing for WS/HTTP (no custom headers) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Runner protocol (versioned BARE schemas, `PROTOCOL_MK2_VERSION`), compatibility: server newer than client |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| State size guidance: actor data ≤ 10 GB (contact Enterprise for more) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — Postgres-backed storage, limits are per-collection quotas (`limits.rs`, `[limits]` config) |
| Transport encodings: BARE (binary), CBOR, JSON over one gateway |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| `c.keepAwake()` for long promises |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — matches do not sleep mid-tick |
| `getGatewayUrl()` from handle for raw HTTP/WS access |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| getOrCreate / get / create / getForId / resolve |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `POST /v1/server/matches` create and `match_join` by id; no idempotent get-or-create by k… |
| Compression |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — grep shows compression only in snapshot/blob paths; no documented WS payload compression… |
| Database-is-the-server model |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — musubi runs server logic as sandboxed WASM modules (`musubi-runtime`, `POST /v1/rpc/{id}`… |
| HTTP/2 backend |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — musubi axum server + QUIC; HTTP/2 only via reverse proxy; CORS not found in repo (grep 0) |
| Lifecycle reducers |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — module hooks on match/world lifecycle and `StatusPresenceEvent`; no module-level connect/… |
| Schedule tables |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Ping location estimation |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Rollout percentage (sticky per user id) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Module canary `--percent` applies to code, not config; Zanshin flags provide % rollouts w… |
| Logical game sessions |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Network protocol checks / version mismatch |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Network time / ticks, update loop |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Relay pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Self-hosted; egress is your cloud bill. |
| Testing with artificial conditions / command-line helper |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |

### 2.10 dedicated servers/orchestration (29 normalized features, 180 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Autoscaling policies (buffer, percentage, schedules, webhook/WASM/metric rules, chain, scale-to-zero, suspend)** |  | ✅ ×7 | ✅ |  |  | ✅ ×4 | ✅ | ✅ ×6 | ✅ ×4 |  |  | ✅ ×2 |  |  |  |  | ✅ | ✅ ×2 | ✅ | ✅ | ✅ |  | 🟡 11/11/5 · P0 |
| **Per-server resource spec (CPU/memory/GPU limits, ports, protocols, env, secrets, grace period, restart policy)** |  | ✅ ×11 |  |  | ✅ ×4 | ✅ ×9 |  | ✅ ×6 | ✅ ×5 |  | ✅ |  | ✅ | ✅ |  | ✅ |  | ✅ ×4 |  |  |  | ✅ | 🟡 12/21/7 · P0 |
| **Game server log capture (stdout/stderr tail, download, retention, signed URLs)** |  |  | ✅ |  |  | ✅ ×3 |  | ✅ ×2 | ✅ |  | ✅ | ✅ |  | ✅ |  | ✅ |  |  |  | ✅ ×2 |  | ✅ | ❌ 2/4/6 · P0 |
| **Hosting providers (Kubernetes/Agones, bare-metal SSH, Firecracker, cloud VMs, Edgegap/GameLift/Hathora adapters)** | ✅ ×2 | ✅ ×36 |  |  |  | ✅ ×4 |  | ✅ ×9 | ✅ ×3 |  |  | ✅ |  |  | ✅ |  | ✅ |  | ✅ |  |  | ✅ ×2 | 🟡 14/19/8 · P0 |
| **Fleet / build upload & versioning (containers, tarballs, registry, image promotion, rollback)** | ✅ ×2 | ✅ |  |  |  | ✅ |  | ✅ | ✅ ×2 |  |  |  |  |  | ✅ | ✅ ×2 |  |  | ✅ |  |  |  | ✅ 6/3/0 |
| **Allocation API (allocate by fleet/pool/region, labels/selectors, fallback order, placement strategy)** |  | ✅ ×28 |  |  |  | ✅ ×2 |  |  | ✅ ×2 |  |  |  |  |  |  | ✅ ×3 | ✅ |  | ✅ |  |  | ✅ ×14 | 🟡 21/21/7 · P0 |
| **Private/BYOC hosts (host lifecycle, enrollment, persistent deployments, host targeting, location strategy)** | ✅ ×2 |  | ✅ ×3 |  |  | ✅ ×5 |  | ✅ | ✅ ×2 |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  |  | ✅ 6/1/2 |
| **Managed relay / IP-hiding gateway / DDoS protection for game servers** |  | ✅ |  |  | ✅ |  |  | ✅ | ✅ |  |  |  |  |  | ✅ |  |  | ✅ ×2 |  |  |  |  | ❌ 1/1/4 · P0 |
| **Deployment / server object model (lifecycle states, status API, tags, metadata, FQDN, TLS termination, context endpoint)** |  |  |  |  |  | ✅ ×16 |  | ✅ ×6 | ✅ ×2 |  |  |  |  |  | ✅ | ✅ ×2 |  |  |  |  |  |  | 🟡 12/13/1 · P1 |
| **Ecosystem add-ons (ingress controller, image pre-pull, fleet GC, image syncer, GitHub Actions runners)** |  | ✅ ×3 |  |  | ✅ |  |  |  | ✅ ×2 |  |  | ✅ |  | ✅ |  |  |  |  |  |  |  |  | 🟡 2/3/2 · P2 |
| **Fleet capacity controls (min available/max per region, availability buffer, manual capacity, utilization charts, density)** |  |  |  |  |  | ✅ |  | ✅ ×4 | ✅ |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  | ✅ ×5 | 🟡 7/6/1 · P1 |
| **Game server self-registration / claiming / direct-to-game instances (no matchmaking)** |  |  |  |  | ✅ | ✅ ×2 |  | ✅ |  |  |  |  |  |  |  |  | ✅ ×5 |  |  |  |  |  | ✅ 7/2/0 |
| **Node-level autoscaling / cloud burst / capacity provisioning (bare-metal first, spot)** | ✅ | ✅ |  |  |  |  |  | ✅ ×4 | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/2/4 · P1 |
| **Per-server metrics (CPU/mem/net/egress/OOM/active connections time series)** |  |  |  |  |  | ✅ ×3 |  | ✅ ×2 |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ | ❌ 0/2/4 · P0 |
| **Server states: Reserved/Ready/Allocated/Shutdown, idle TTL reaper, max duration** |  |  |  |  |  | ✅ ×2 |  |  |  |  | ✅ | ✅ |  |  |  |  |  | ✅ |  |  |  |  | ❌ 2/1/2 · P1 |
| **Game server SDK (ready/health/allocate/shutdown/label/connect/players, watch stream, native libs)** |  | ✅ |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  | 🟡 0/3/0 · P0 |
| **Health policy (initial delay, period, failure threshold, disabled) and disruption/eviction semantics** |  | ✅ ×2 |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 1/3/0 · P1 |
| **Rolling updates / build canary / promote with maxSurge/maxUnavailable, rotate Ready servers** |  | ✅ ×3 |  |  |  | ✅ ×2 |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 2/3/0 · P1 |
| **Counters / Lists / per-server metadata (capacities, allocation filters, autoscaler targets)** |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 1/1/0 · P1 |
| **Deployment / gameserver lifecycle webhooks and events (ready/error/terminated)** |  | ✅ |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/2/0 · P1 |
| **Infrastructure-as-code / declarative resources (CRDs, Terraform provider, kubectl-apply model)** |  | ✅ ×5 |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 1/2/2 · P1 |
| **Region catalog, locations, latency map, data-center metadata** |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  | ✅ 3/0/0 |
| **Rooms-per-process packing / multi-match per server** |  |  |  |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/2/0 · P1 |
| **Server browser query protocols (SQP/A2S) and server.json config** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ | 🟡 0/1/0 · P2 |
| **Server inventory / search API (filter by region/status/age, counts, exit reason)** |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 1/1/0 · P1 |
| **Container groups / sidecars / dependencies / per-instance groups** |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/1/0 · P2 |
| **Multi-cluster / multi-region / multi-supplier allocation with priorities and fallback** |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Per-player session creation limits and scale-down protection** |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Server-side kick / player auth on server / per-server player list** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×3 |  |  | ✅ 3/0/0 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Local DS registration (AMS Simulator cloud-integrated), CI/CD upload automation, multiple AMS accounts linkin… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Public Cloud game CCU quota 25,000 (403 above quota unless login queue; slider to set lower; raise on request) | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no CCU quota; `login_queue_capacity` configurable |
| Session↔DS: deployment configs in templates, requested regions, DS Hub WS notifications, custom DS (bring-you… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Support: Sev1 15 min response / 8h target resolution, Sev2 15 min / 24h, Sev3 8h Standard / 2h Professional /… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — community/OSS support only |
| Cluster creation guides: GKE (Standard/Autopilot, release channels), EKS (eksctl), AKS, OKE, minikube (multi-… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Terraform modules assume an existing cluster except Hetzner/OVH (which create VMs); no cl… |
| Confirm-installation checklist (6 pods: allocator×3, controller×2, extensions×2, ping×2) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Container OS/arch matrix: linux/amd64 stable, linux/arm64, windows alpha |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Rust binary builds for linux amd64/arm64; no documented arch matrix for game-server images |
| Dual-stack / IPv6 load balancer settings (`ipFamilyPolicy`, `ipFamilies`) for ping and allocator services (1.… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — IPv6 addresses accepted in addresses (`rg -i ipv6` → 2 hits) but Helm Service has no fami… |
| Duplicate `GameServerSet` cleanup, fleet rolling-update replica clamping fixes (1.57, 1.61) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — No GameServerSet concept |
| Fleet name ≤ 63 chars; validation webhooks reject bad specs |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Server-side validation of fleet payload exists (`FleetSpec` validation in fleets.rs); no… |
| GameServer `kubectl describe` events for each lifecycle transition (`Starting`, `Ready`, `Unhealthy`, …) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/server/events` + webhooks deliver server events; no per-object event history |
| GameServer creation/deletion throttles: `maxCreationParallelism`, `maxGameServerCreationsPerBatch`, `maxDelet… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — Reconciler sizes the buffer every 5 s with no configurable batch limits |
| Integration pattern: reuse an Allocated server for the next session by calling `Ready()` again |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Persistent controller logs volume (`controller.persistentLogs`, size limit) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Logs to stdout (tracing) |
| Policy `Chain`: ordered list of `{id, type Schedule/Webhook/Buffer…}` evaluated by priority with fallback |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — Single policy per fleet |
| Policy `Wasm` (alpha 1.54, Extism): module from `service`/`url` + `caBundle`, sha256 pin, `function` (default… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — musubi has a WASM module runtime (`musubi-runtime`) but it exposes no fleet-scaling hook… |
| Quilkin UDP proxy integration (Embark) for routing/filtering in front of GameServers |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi has its own UDP/QUIC edge for its realtime transports (README:realtime transports)… |
| Response `status.state` = `Allocated` / `UnAllocated` / `Contention`; `gameServerName`, `address`, `addresses… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Response has server id, address, port, `connect_token`; `wait_secs` long-polls instead of… |
| Stable network identity: headless Service + `subdomain` gives each GameServer a DNS name |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — Servers addressed by IP:port only |
| `Fleet` CR: `replicas`, `template` (GameServer spec), backed by `GameServerSet`s |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `allocationOverflow.labels/annotations` applied to Allocated servers that exceed desired replicas after scale… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — Scale-down ignores allocated servers (they finish and exit); no marker is applied |
| `controller.numWorkers` per resource type, `apiServerQPS`/`apiServerQPSBurst` throttles |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — No apiserver |
| `players{minAvailable,maxAvailable}` filter (alpha PlayerAllocationFilter) — removed 1.59 in favour of Lists |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `remoteAllocationTimeout`, `totalRemoteAllocationTimeout`, `apiServerQPS/Burst` for the allocator |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `wait_secs` on the request; no per-service timeout tuning |
| `spec.counters{key:{count,capacity}}` declared at creation (default capacity 1000, keys immutable afterwards) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Only one implicit counter: `players`/`capacity` on `game_servers` (mig 0067, `POST /v1/ga… |
| `spec.lists{key:{capacity,values[]}}` (string lists, capacity ≤ `gameservers.lists.maxItems`, default 1000) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No per-server list state; labels JSON (`POST /v1/gameserver/label`) is free-form but not… |
| `status.address` picked by priority ExternalDNS > ExternalIP > InternalDNS > InternalIP; `status.addresses[]`… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `address` is a single string set by the provider (`public_addr` for ssh, node IP for k8s)… |
| `status.nodeName`, `status.reservedUntil`, `status.players`, `status.counters`, `status.lists`, `status.ports… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `node_id`, `address`, `port`, `players`, `capacity`, `labels`, `allocation`, `build_versi… |
| `sync.type: FixedInterval` with `seconds` (default 30) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Reconcile cadence fixed at 5 s (fleet/lib.rs), not per fleet |
| Defensive rate limiting |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Monitoring / on-call |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Operator-run |
| 0.17 full-stack typing: declare `client.auth`/`client.userData` types once on backend, infer room message/pay… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — OpenAPI (`/openapi.json`) + IDL codegen give typed contracts, but no single-language end-… |
| Default presence/driver auto-selection: Redis chosen when `os.cpus().length > 1 \|\| REDIS_URI`, else local (… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — cluster membership is always on; single node works with same config |
| Matchmaker drivers: `LocalDriver` (default), `RedisDriver` (ioredis/Cluster), `DatabaseDriver` (`colyseus_roo… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Matchmaking `driver` (room cache/query store): LocalDriver, RedisDriver, MongooseDriver, DrizzleDriver |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Pluggable `logger` (pino/winston), `greet` banner toggle |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `tracing` structured logs + OpenTelemetry; not pluggable to arbitrary JS loggers (n/a in… |
| Presence API: `LocalPresence` (default) / `RedisPresence` (ioredis, Redis Cluster): pub/sub `subscribe/unsubs… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — modules get `kv` host ops (get/set/cas/ttl via Postgres) + cluster event bus; no Redis-li… |
| TIMED protocol envelope (server time + per-recipient last-input-ack stamped on ROOM_STATE_PATCH for input roo… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Transport connection lifecycle: parse `sessionId`/`roomId`/token from URL, heartbeat, forward to room via mat… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Transport: Bun WebSockets (experimental) with Express compat layer |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — single Rust binary |
| Transport: WebSocket default (`ws`) with `pingInterval` (3000 ms) / `pingMaxRetries` (2) heartbeat, `maxPaylo… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WS `GET /ws?token=&format=json\|binary`, ping/pong + `rtt`, `[anticheat] max_input_bytes`… |
| Type-safe HTTP routes `createRouter()/createEndpoint()` (shared types to SDK) + classic `express` callback fo… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — custom server logic exposed only as `POST /v1/rpc/{id}` (WASM, typed via IDL `[rpcs.*]` →… |
| Typed HTTP routes `createEndpoint/createRouter/createMiddleware` (ctx.context merge), `basicAuth` middleware… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — custom logic exposed via WASM `rpc` exports (`POST /v1/rpc/{id}`) with OpenAPI; no arbitr… |
| Vite plugin: run server inside Vite dev server (Environment API), single process for client+server, `vite bui… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Rust binary; `musubi dev` single-process dev loop |
| `COLYSEUS_LATENCY=<ms>` env + `server.simulateLatency(ms)` (dev only, must not run in prod) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg -i 'simulate_latency\|latency_ms' crates` → only bench/soak tooling, no server-side a… |
| `auth` option mounts `@colyseus/auth` routes (`oauth`, `prefix`) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `beforeListen` async hook (connect DB first) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — built-in Postgres bootstrap, `musubi doctor` |
| `database` option boots `@colyseus/database` and contributes default routes (auth endpoints) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `defineRoom(RoomClass, defaultOptions)` — same class under several names with server-side option override |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/server/matches {module, params}` / IDL `[matches.<module>] params` validation;… |
| `isStandaloneMatchMaker` process role (matchmaking only, room creation via IPC) |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `presence` (LocalPresence / RedisPresence) — inter-process pub/sub + KV needed to scale |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `server.removeRoomType()` at runtime |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `server.simulateLatency(ms)` dev helper |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no latency injection switch (bench/soak only) |
| Deploy to fleet API |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `provider = ssh` fleets with host list; no overflow to a second provider |
| Deployment "users" telemetry |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Allocation carries `payload` (opaque); players reported by server via `/v1/gameserver/pla… |
| Deployment Agent sample (stop + read env from server) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Game-server SDK is HTTP only (no engine sample) |
| Deployment map + balance-point heatmap |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No UI; `GET /v1/server/fleets/{id}/demand` gives demand history as JSON only |
| Deployment rate limit |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| External registry guides |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Docker/k8s providers accept any image but musubi stores no pull credentials |
| Fleet API provisioning |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Image source: Edgegap registry / Docker Hub / private registry profile with credentials |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `spec.image` is a plain reference passed to docker/k8s/edgegap provider; no stored regist… |
| Image tag immutability guidance (`latest` discouraged; dashboard warning) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi does not cache images itself; providers pull per their own policy |
| Image verification |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No image pull validation on `fleet push` |
| Legacy Smart Fleets (policy engine) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Nakama on Managed Cluster recipe |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Pull profiles (files from S3 into container at start) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Content bundles / storage objects exist for players; no file injection into game-server c… |
| S3 Endpoint Storage |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg -i s3` → none |
| Session configuration on version |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GameServer.capacity/players`, backfill allocation; no socket-accounting session objects… |
| Stop deployment / bulk stop |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `DELETE /v1/server/gameservers/{id}`, `POST /v1/server/fleets/{id}/scale`; no filter-base… |
| Anywhere fleet cost attribute |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no cost attribute on fleets |
| Anywhere fleets (customer hardware) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `ssh` provider (bare metal), webhook/external providers; no self-registering compute API… |
| Anywhere roadmap & creation |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — ssh provider automates; no registration flow for externally-started servers except `POST… |
| Build object & console |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `FleetBuild{version, spec, state, canary_percent, notes, created_at}`; no binary/size/OS |
| Build packaging guidance |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Build resources |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no artifact store: builds are specs referencing an image/command (`FleetBuild.spec`), no… |
| Container fleet build walkthrough |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Container remote access |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator owns cluster |
| Containers starter kit / wrapper |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `FleetSpec.command` runs any binary but it must call `/v1/gameserver/ready` to serve |
| Custom build customisation (game properties, SDK config) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Custom locations (Anywhere) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Edit fleet attributes |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/server/fleets/{id}/scale` + build updates; name/labels editable, no protection/… |
| Fast build update / multi-build tooling |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — versioned builds + canary per fleet, but no binary distribution |
| Fleet creation lifecycle |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — fleets are `active` immediately, builds have `state` (`FleetBuild.state`), no multi-phase… |
| Fleet deletion semantics |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Fleet metric groups |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus labels by fleet/region/pool; no explicit metric groups |
| Fleet port settings |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `port_start/port_end` per fleet; no CIDR allow-list on fleet |
| Game session protection |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — reconciler reaps only idle/unhealthy servers (`musubi-fleet` reconcile every 5 s); no exp… |
| GameLift Agent |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no agent for BYO hosts (ssh provider pushes processes; no pull-based agent) |
| GameLift Local (SDK 3.x/4.x emulator) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — not needed – single binary is the real server |
| Getting started examples |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — examples in repo `examples/`? not verified |
| Hosting solution management |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Install script |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no install hook; Dockerfile responsibility |
| Key terms / how it works / flavors |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Local iteration with Anywhere |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Multi-Region resilience / DR |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Regions & stream-class locations |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Remote access to instances |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator owns hosts; `ssh` provider is the only host access path |
| Scale to / from zero |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Scheduled / predictive scaling |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Supported game types / engines |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Vulnerability analysis & AMI patching |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator rotates images |
| Windows game servers |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `local`/`ssh` providers can run Windows binaries? not verified → partial; docker/k8s prov… |
| 14 regions |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — regions are wherever the operator deploys (`[regions]`, fleet regions); Terraform modules… |
| Build expiry / delete with safety warning |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Build logs + share URL |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no managed image builds |
| Build metadata in Console upload |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no console yet (README status) — CLI `musubi fleet push` sets version/notes |
| Create room against a specific deployment |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — canary share by percent/region picks build at server start; cannot pin a single allocatio… |
| Eager build caching / baked node images |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `buffer` ready pool hides cold starts; no image pre-pull orchestration (provider-dependen… |
| Fleet reusable across apps/environments |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| GetDeployments / GetLatestDeployment / GetDeployment |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Global builds (shared across apps) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — promote/rollback within a fleet (R194); same image can be referenced by several fleets, b… |
| IPv4 host:port connection info |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Load-balanced applications |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — generic HTTP ingress for arbitrary services is not a game-backend feature (musubi's own H… |
| Ping/Discovery service |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Private backbone / edge routing |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — UDP edge proxy + georouting exist in-cluster; no global anycast backbone (needs operator'… |
| Process object |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — game server rows have fleet/region/state/build_version/labels/ready_at/terminated_at (R19… |
| Region-aware matchmaking input |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Regional control planes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Scheduled scaling |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| TCP/UDP on same port |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Up to 3 exposed ports per container |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Inventory server ops: Alter Player Inventory (grant/remove), character & hero inventories, universal assets,… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `/v1/server/inventory/grant`, consume, instances; no character/hero-scoped inventories |
| Player Files server-side: list/get/delete by id or key, upsert by key, file revisions (list, get, promote rev… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No per-player file store with revisions (storage values are JSON ≤ configurable size; UGC… |
| Player Storage multi-player ops: lookup keys for multiple players, set/delete for multiple players in one call |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Server API surface: Session start + Ping keepalive, Leaderboards (schedules, rewards, archives download, manu… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client transport: persistent TCP with TLS at load balancer, WebSocket for WebGL; UDP only for multi-region ga… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Database scan jobs (batch wake all entities of a type for migrations/maintenance; dashboard progress) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — retention/cleanup jobs + storage migrations run server-side; no generic entity-scan job f… |
| Public Web Endpoints: separate ASP.NET public HTTP server routed to `publicWebApi` nodes, for webhooks/callba… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Queuing server actions to offline players (executed when actor wakes); custom entities how-to; entity-to-enti… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WASM hooks + scheduled logic + deferred rewards; no "queue action against offline player… |
| Asset delivery via S3 pre-signed URLs through RPC (pattern); remote configuration via Satori or storage objec… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| LoadBalancing app |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Multi-Peer mode |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — `musubi bench`/`soak` spawn many simulated clients; multi-match per node is native (actor… |
| NameServer app |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Photon Server v5/v6 (self-hosted) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| PhotonControl |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Plugins on .NET Framework vs .NET (v6) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Policy file listener, S2S, HTTP routing |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — internal node bus (Postgres LISTEN/NOTIFY, SWIM) instead of S2S config |
| Assets & certs APIs: GetAssetUploadUrl, GetAssetDownloadUrl, ListAssetSummaries, DeleteAsset, UploadCertifica… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — Builds reference container images/artifacts you host; no asset upload/cert store |
| Core quota management: per VM family × region core quota, quota summary with peak usage charts/CSV export at… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Self-hosted: capacity = your cluster; musubi exposes vCPU-hours usage metrics only |
| Deploy tooling: Game Manager wizard, PlayFabMultiplayerApi PowerShell module (New-PfBuild, Request-PfMultipla… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Actor metadata (`c.actorId`, `c.name`, `c.key`, `c.region`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Backups: `pg_dump`, stop-and-tar RocksDB, FDB backup agent (EE), restore test checklist |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — Postgres-native backups implied; `musubi export`/`import` for data (`transfer.rs`) and pe… |
| Config formats JSON/JSON5/JSONC/YAML + `RIVET__SECTION__KEY` env overrides |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Error masking: internal errors hidden from clients unless `RIVET_EXPOSE_ERRORS=1` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Horizontal scaling: scheduler places actors across runners by slots; no shared-state coordination needed |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Pub/sub backend for multi-node (NATS with replicas for HA) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Single Rust binary control plane: schedules, routes, persists, never runs user code |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Storage backends: filesystem/RocksDB (single node), PostgreSQL (multi-node, ~1,000 concurrent actors, no PgBo… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — Postgres only (+ Raft in `musubi-cluster` for metadata); no embedded single-node store an… |
| TLS guide (reverse proxy / cert termination) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Backups / PITR |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — no built-in backup tooling (grep `backup`, `pitr` = 0 hits); relies on Postgres backups;… |
| Multi-node self-hosting / HA |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Pause / resume database |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — (musubi hibernates cold cells / scale-to-zero fleets instead) |
| Regions |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Zero-downtime module update |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Distributing the dedicated server |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — musubi ships server builds through fleets (`musubi fleet push`, builds canary/promote/rol… |
| Game server public list via Web API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Secure (VAC) servers |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — anti-cheat signals + EAC/BattlEye ticket verification + automatic kicks (README Anti-chea… |
| Server reputation / gameplay stats |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — no server reputation score |
| Server restart / update request |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Server-set user stats/achievements |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| API auth |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| API: image jobs (install, update, difference analysis, rollout status, version status, options) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Build versions + canary/promote/rollback; no image job pipeline. |
| API: locations / countries / regions listing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| API: reservations reserve/unreserve on machine |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| API: servers list / status / start by criteria / stop / restart |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Auto scaling / capacity failure handling |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Nodes scale via Helm/Terraform + cluster membership; relayed-match capacity is your node… |
| Builds from S3 / GCS; releases |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Fleet builds are versioned `spec` (image/command/env/vcpu) via `musubi fleet push` (READM… |
| CLI (`mpcli`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Differential (delta) image updates + mirror network |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — Container images / binaries pulled by provider; no delta image pipeline. |
| Fleet profile switch (Clanforge) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Legal: ToS, AUP, cookie notice, sub-processors, DPA |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Apache-2.0. |
| Machine report / server start-stop-restart-save actions |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Server stop/scale via fleet routes; no "save" action. |
| Matchmaker integration patterns |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Privacy / DPA |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Self-hosted; musubi ships GDPR export/delete + COPPA age gates/parental consent (README "… |
| Regions (11) + selection heuristics |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Reservations (server-browser model) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 — Allocation-only model; `GET /v1/matches?label=&open_slots=` gives a server browser for au… |
| Rollout modes progressive (empty servers only) vs forced (kick) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — Build `canary_percent`/`canary_regions` → `promote`/`rollback` (README 5.x) gives staged… |
| Secret management in Multiplay dashboard |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Config/env. |
| Test allocations from dashboard |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — API allocate works for tests; no UI. |
| Zero-downtime A-B build configurations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |

### 2.11 meshing/world (13 normalized features, 92 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Authority handover, ghosts, hysteresis, group handover** |  |  |  |  |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  | ✅ |  |  |  | ✅ | ✅ 4/2/0 |
| **Interest management (radius, multi-query, tag/global queries, per-query frequency, byte budgets)** |  |  |  |  |  |  |  |  |  |  | ✅ ×5 |  |  |  | ✅ |  |  |  |  |  |  | ✅ | 🟡 3/3/1 · P1 |
| **Simulated player load tests / deployment load testing** |  |  |  |  |  |  |  |  | ✅ |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 2/2/0 · P1 |
| **Spatial partitioning (grid/quadtree/octree, 3-D cells, split/merge by load)** |  |  |  |  |  |  |  |  |  |  | ✅ ×5 |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 1/4/1 · P1 |
| **World inspector / spatial debugger / worker load view** |  |  |  |  |  |  |  |  |  |  | ✅ ×3 |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ 0/0/3 · P1 |
| **Entity caps / per-client quotas / memory-pressure admission** |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Network LOD / per-field quantization / per-entity replication rate** |  |  |  |  |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/2/1 · P2 |
| **Non-spatial partitioning / offloading by entity type or layer** |  |  |  |  |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  |  |  |  |  |  | 🟡 2/1/1 · P2 |
| **Published scale benchmarks (CCU per shard/world)** |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  | 🟡 1/1/0 · P2 |
| **Simulator / worker SDK (Rust sim SDK, Unity/Unreal GDK, streaming protocol)** |  |  |  |  |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  |  |  |  |  |  | ❌ 1/0/2 · P2 |
| **Typed commands / RPC between entities and workers (routing, ordering, deadlines)** |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Worker scheduling / placement strategies / load balancing of cells** |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 1/0/0 |
| **World persistence, snapshots, journal, time-machine, cold cells** |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ 1/0/0 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Global Entities (app-wide, ACL, TTL, indexed id, system entities) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| User Entities (JSON docs, versioned, ACL, singleton) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| 3-D space *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `musubi-mesh/src/lib.rs:8` "Space is 2D here (x, y); 3D/octree ... later" |
| Asset management *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — engine-side |
| Authority models *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — server/simulator authority only (lease holder writes); clients send `world_input`; no cli… |
| Authority transition along travel path *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Autosimulator connection *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| CI integration *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| CLI + REST gateway *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Cell lifecycle callbacks *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Chunk-based interest (deprecated) *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Clients see across worker boundaries *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Commands (RPC) *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `interact` (seq-ordered, routed to owner) and `POST /v1/rpc/{id}` server RPCs; no client→… |
| Component delivery / filters *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — subscribe-time `filter` only; no per-component reliability class |
| Cross Session Persistence *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Cross-server possession *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Custom bindings / descriptor providers *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — engine-side |
| DDoS note *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Database Sync Worker *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Deployment Launcher / Mobile Launcher *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Deployment Manager *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Design guides *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — README/parity only; no design guides |
| Disconnect timeout / keep-alive *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Distributed debugging *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — per-world history/time-machine + request record/replay, no live distributed debugger |
| Dynamic scale *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| ECS update sets *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Engine support *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — no Unity SDK; Unreal scaffold not CI-compiled; Rust sim SDK only |
| Entities leaving query destroyed/pooled *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Entity Component Update Scheduler *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — per-client component filter and relevance ranking, but no distance-based per-component ra… |
| Entity Graph *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — attach trees (root/passengers) only |
| Entity ID reservation pool *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — UUIDs, no reservation needed |
| Entity Spawn Batches & Snapshots *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Entity database + snapshots *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| EntityTemplate / ACL write access *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — authority = lease holder; no per-component write ACL |
| Fragmented channel for big data *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Functional tests manipulating authority/interest *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Host Authority: restrict entity creation *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Host Authority: validate connection *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| IPv4 only *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Interest *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — radius+filter+budget; no always-interested / owner-only flags |
| Local = cloud parity *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Max persistent objects *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Max synced variables per component *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Multi-cloud *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Multi-worker debugging *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — OTLP tracing + per-world history; no engine debugger integration |
| Multiple connections per game instance *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — SDK concern; server allows many sessions per device |
| Network Analyzer *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus fan-out metrics only |
| Network FPS *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Networked physics *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — physics explicitly out of core (user decision); neither shipped |
| Non-game simulation *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — game backend scope |
| Online platform layer *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Orphans & auto-adopt *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Parenting / nested entities *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Per-field sampling frequency at runtime *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — per-client `max_hz` only; no per-component rate |
| Persistence services *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Persistence survives RS restart *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player Lifecycle module *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Portal *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no console UI |
| Process spawning primitive *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — different abstraction (actors on cluster nodes, `musubi-actor`) |
| Provisioning *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| RS REST auth *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Remote logs *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Replication hierarchy *(Flax Engine high-level networking)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — per-client budget/relevance ranking; no per-object replication rate |
| Run local simulators / editor as simulator *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Runtime = entity DB + bridge + load balancer *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Runtime capacity *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Schema language *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — TOML IDL for API/match types; world components untyped |
| Server frame scale *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — 30 Hz world ticks per thread; soak at 10k entities per world |
| Server-side OCS (freeze & serialize) *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Serverless positioning *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — musubi runs on k8s/VMs by design |
| Shard model *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Simulator build & upload *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi fleet` builds canary/promote/rollback; no Unity build pipeline integration |
| Simulator query transfer *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — musubi interest is server-evaluated per client; no client-owned query entity |
| Simulator secret payload *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Simulators *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Snapshots *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| State vs Input authority split *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — input routed to authoritative sim (`world_input{entity}`), but no explicit per-entity inp… |
| Static server meshing *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Transfer types *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — handover is cell/placement-driven (`handover/ack`, co-locate pin); no client request/stea… |
| Transform Synchronization *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `pos_quantum` only |
| Unique Global Entity ID *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Uniqueness *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — entity UUIDs; idempotent spawn by id in tx; no named-singleton replace semantics |
| Web/WebGL *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Write access authority per component *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — authority is per entity (lease holder writes all components); no per-component ACL |
| phtree-cpp *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — |
| Clustering (gossip mesh, split-brain recovery, uniform nodes), LTS releases + hot fixes — commercial license… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Behaviour Interest |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Entity prototypes & asset DB |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| NetworkTransform |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — world entity transform replication in mesh; nesting/parent sync absent |
| Actor per entity / coordinator & data actor patterns |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — no equivalent; musubi models entities as rows in Postgres subsystems (accounts, groups, c… |
| Scale-out / replication |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Server meshing (one world across many nodes) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |

### 2.12 economy/IAP (19 normalized features, 59 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Store / catalog with per-platform listings, bundles, rotation, discounts, coupons, requirements** | ✅ ×6 |  | ✅ ×4 | ✅ ×3 |  |  | ✅ ×2 |  |  | ✅ ×3 |  | ✅ ×2 | ✅ ×6 | ✅ ×2 |  | ✅ ×7 | ✅ ×3 |  | ✅ | ✅ ×4 | ✅ ×4 |  | 🟡 20/20/5 · P0 |
| **Virtual currencies, wallets, ledger, idempotent adjustments** | ✅ ×9 |  | ✅ ×5 | ✅ ×8 |  |  |  |  |  | ✅ ×7 |  | ✅ ×4 | ✅ ×6 | ✅ ×3 |  | ✅ ×9 |  | ✅ | ✅ | ✅ ×7 | ✅ ×6 |  | ✅ 41/19/5 |
| **Items catalog & inventory (stackable, instances, equip, consume, versions, limits)** | ✅ ×5 |  | ✅ ×4 | ✅ |  |  |  |  |  | ✅ ×2 |  | ✅ | ✅ ×6 |  |  | ✅ ×8 | ✅ ×3 |  | ✅ | ✅ ×16 | ✅ ×2 |  | ✅ 29/14/4 |
| **Subscriptions (renewals, grace, server notifications, status)** |  |  | ✅ | ✅ | ✅ |  |  |  |  | ✅ ×2 |  | ✅ |  | ✅ ×3 | ✅ |  |  | ✅ | ✅ ×4 | ✅ ×3 |  |  | 🟡 4/9/3 · P0 |
| **Real-money checkout / PSP integration (Stripe, Xsolla, PayPal, Adyen, hosted payment page, webshop, tax/invoices/refunds)** | ✅ ×3 |  |  | ✅ |  |  |  |  |  | ✅ |  | ✅ |  |  |  | ✅ |  |  |  | ✅ ×3 | ✅ |  | ❌ 4/2/5 · P0 |
| **IAP providers beyond the basics (Amazon, Microsoft Store, Facebook Instant, Meta Horizon, Epic, PSN/Xbox entitlements, Discord, Unity IAP)** | ✅ |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  | ✅ ×2 |  |  |  |  |  | ❌ 1/2/2 · P1 |
| **IAP receipt validation: Apple/Google/Steam/Huawei/Samsung** |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  | ✅ |  | ✅ ×2 |  |  |  | ✅ ×2 |  |  | ✅ 5/1/1 |
| **Offers & placements (offer groups, priority, cooldown, precursor chains, per-player limits, purchase intents)** |  |  |  |  |  |  |  |  |  |  |  | ✅ ×3 | ✅ |  |  |  |  |  |  | ✅ |  |  | 🟡 1/2/1 · P0 |
| **Reward tables, drop tables, loot rolls, reward modifiers, grant limits** | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  | ✅ |  |  |  |  |  |  | 🟡 2/3/0 · P1 |
| **User-generated content (upload, moderation queue, ratings, subscriptions, share codes, dependencies, versioning)** | ✅ ×2 |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ ×6 |  |  | 🟡 4/3/1 · P1 |
| **Economy configuration workflow (draft/publish, catalog versions, per-locale strings, images, reviews)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |  | 🟡 1/1/0 · P1 |
| **Energy / stamina / time-gated resources** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×3 |  |  |  |  |  | ✅ |  |  |  | ❌ 1/1/2 · P1 |
| **Item lifecycle & effects (publish/retire/revoke states, cooldowns, timed status effects, durability)** |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |  |  |  | 🟡 0/1/0 · P1 |
| **Marketplace / auction house / order book** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ ×3 |  |  | ✅ |  |  |  |  |  |  | 🟡 1/2/1 · P1 |
| **Redemption codes / key campaigns / platform key inventories** | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/1/1 · P1 |
| **Rewarded video / ad placements with S2S verification** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |  |  |  |  |  |  | ❌ 0/0/2 · P1 |
| **Trading / player-to-player exchange / escrow** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |  |  | ✅ 2/0/0 |
| **Entitlements / DLC / platform ownership sync** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 0/1/0 · P2 |
| **Settlement / revenue reports / reconciliation feeds** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 0/1/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Cloud Save Portal data management |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Admin storage routes exist; no UI; musubi has storage history/versions (ahead on restore) |
| Commerce `stats-update` & purchase status |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Purchase hooks via modules |
| Content snapshots in Git / CLI snapshot & restore |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Content bundle versions + export/import to files; no snapshot/restore CLI workflow |
| Deprecated item types & pruning |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| VIP tiers (`VipContent`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No VIP tier system (grep vip hits are unrelated) |
| Blockchain (Ultra.io) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — partner-specific |
| Platform ID catalogue |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — platforms tracked for store/entitlements/crossplay, not a canonical client platform enum |
| Lifecycle hooks: onCreate, onAuth (instance or static, ctx token/headers/ip), onJoin(client, options, auth),… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WASM exports init/join_attempt/join/leave/tick/terminate; disconnect-vs-leave is handled… |
| Persistence guidance |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Config items/libraries/archives model: `#key` columns (compound keys), one sheet per library, binary `.mpa` a… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi content bundles (`/v1/admin/content/*`: versions, diff, rollback, environments, dr… |
| Item references `MetaRef`/`MetaConfigId`, cross/recursive references, late resolution, reference analyzer, it… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no typed reference graph in content documents |
| Player-authored content storage, generic collections with JSON Schema, object storage |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Sandbox purchases: Apple sandbox receipts, Google test accounts, dev platform fake purchases gated by `Enable… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Side-by-side diff of any two versions, search by item id, swap baseline; "Diff to Active" |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/admin/content/diff` JSON diff only |
| Disabled items hidden from all APIs; keep_zero |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — item `enabled` flag exists; no keep-zero (show owned-before items at 0) semantics |
| Donations (ask friends/team for items) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — rg -i donation in musubi -> none (group treasury/gifts exist but no player-to-player requ… |
| Fake receipts for dev |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Item sets in rewards (set intersection, max_repeats, only items available to player) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi reward entries reference items/currency/xp/tables; items have `tags`/category but… |
| Offline sync (BASE_SYNC, device prefs) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — no offline-first client reconciliation (no SDK) |
| Purchase restore (`ECONOMY_PURCHASE_RESTORE`) and placement status/success/fail RPCs |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi `GET /v1/iap/receipts` lists validated receipts for restore; no placements |
| Removing items: negative grant vs DeleteItems by instance id |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Reward config: guaranteed + weighted tables |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Reward destinations: player / mailbox / team / team mailbox / team members |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — README "Deferred rewards": `deferred: {delay_secs, ttl_secs, title, body}` freezes outcom… |
| `SetConfigSource` per-item fallback lookup for items unknown at deploy time (procedural / external) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi items are DB rows creatable at runtime via `POST /v1/server/economy/items`; no laz… |
| Steam purchases — not supported |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Host migration (Host mode) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Player TTL / rejoin |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Catalog APIs: CreateDraftItem, UpdateDraftItem, GetDraftItem(s), DeleteItem, PublishDraftItem, GetItem(s), Ge… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — README items/store admin routes (`/v1/server/items`, `/v1/store`) — no draft/publish, no… |
| GetUserData / UpdateUserData (client, Permission Public/Private, ≤10 keys per call, 300KB value, 10 updates p… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Legacy timed consumables (UsagePeriod/UsagePeriodGroup expire by time), limited-edition items (InitialLimited… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Content Data catalogs |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — content bundles/environments (`GET/POST /v1/admin/content`, README "Content bundles") cov… |
| Content versioning & live apply |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Game Instance DataStore |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Order statuses & records |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Player order history (operator) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — admin routes list receipts/ledger per account; no UI. |
| Unsafe order provider (testing) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Xbox / PlayStation commerce |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| CLI live subscribe |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — no CLI tail of realtime data |
| Client-side cache |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — no client SDK with local state mirror (SDKs "not written yet", README) |
| Typed query builder (client) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — no client query language |
| Dependencies & collections |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — no UGC dependency graph (`rg -i "ugc.*depend"` → 0) |
| Inspect item link |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — no inspect/share links |
| Item create/update |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Market popularity / asset lookup |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Promo items |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — entitlement-driven grants (Steam/PSN/Xbox ownership sync → entitlements) can gate rewards… |
| Publish/update via Web API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Published item search (legacy) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Sandbox |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Stacks |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Visibility states |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Votes & favorites |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Access classes: Default (player r/w), Public (player-write, anyone-read), Protected (server-write, player-rea… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Editor / CLI / dashboard / REST authoring + deployment of economy resources |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — `musubi deploy` manifest + content bundles; no editor integration |
| Game Data (custom items keyed by `customId`, client read-only, server write) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Postgres disk you own |
| Total configuration items cap |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Virtual purchases (costs ≤20 items, rewards ≤20 items) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Xbox purchase verification via Cloud Code module sample |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Relay via `NetworkDriverStore` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |

### 2.13 progression/LiveOps (15 normalized features, 48 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **A/B experiments (variants, sticky assignment, phases, significance/scorecards, exclusion groups)** |  |  | ✅ ×3 |  | ✅ ×2 |  | ✅ |  | ✅ |  |  | ✅ ×7 | ✅ ×2 |  |  | ✅ ×5 |  |  | ✅ |  | ✅ |  | ❌ 1/9/11 · P0 |
| **Progression tracks / XP / levels / skill trees / timed unlockables** | ✅ ×3 |  |  | ✅ ×3 |  |  | ✅ |  | ✅ | ✅ ×7 |  |  | ✅ ×3 | ✅ ×4 |  |  |  |  | ✅ |  | ✅ |  | 🟡 12/11/1 · P0 |
| **Segments / audiences (rule builder, tracked membership, enter/exit actions, scheduled refresh, composable algebra)** |  |  | ✅ | ✅ ×2 | ✅ |  |  |  |  | ✅ ×6 |  | ✅ ×4 | ✅ ×5 |  |  | ✅ ×11 |  |  | ✅ |  | ✅ ×4 |  | 🟡 2/26/7 · P0 |
| **Rules engine / triggers / automation (event → action with filters, scheduled tasks, no-code)** |  |  | ✅ ×2 |  |  |  |  |  |  | ✅ |  | ✅ |  |  |  | ✅ ×6 | ✅ ×2 |  |  |  | ✅ ×8 |  | 🟡 7/12/1 · P0 |
| **Campaigns / operator messaging (mail/push/email templates, targeting, schedules, localization, journeys)** |  |  | ✅ ×3 | ✅ |  |  |  |  |  | ✅ ×2 |  |  | ✅ ×2 |  |  | ✅ |  |  |  |  |  |  | ❌ 2/2/4 · P0 |
| **Feature flags / remote config variants with targeting (rollout %, conditions, scheduled overrides)** |  |  | ✅ |  |  |  |  |  |  | ✅ ×2 |  | ✅ ×3 |  |  |  |  |  |  |  |  | ✅ ×2 |  | ❌ 0/3/5 · P0 |
| **Live events (scheduled server-owned events with phases, templates, local time, targeting)** |  |  |  |  |  |  |  |  |  |  |  | ✅ ×5 | ✅ ×13 | ✅ |  |  |  |  |  |  | ✅ ×2 |  | 🟡 2/10/9 · P0 |
| **Quests / missions / challenges (daily/weekly, rerolls, rotations, randomized goal pools)** | ✅ ×2 |  |  | ✅ |  |  |  |  |  |  |  | ✅ | ✅ ×2 |  |  |  |  |  |  |  |  |  | 🟡 3/3/0 · P1 |
| **Streaks / daily login rewards / calendars** |  |  | ✅ |  |  |  |  |  |  | ✅ |  |  | ✅ ×2 | ✅ |  |  |  |  |  |  |  |  | 🟡 2/3/0 · P1 |
| **Seasons / battle pass / season pass (tiers, premium, clone/export)** |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |  | ✅ 2/1/0 |
| **Deterministic shared client/server game logic model (client-first execution, server re-validation)** |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Discord bot / community integrations (roles as rewards, claimable rewards)** |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Maintenance mode / developer-player flag / server time-skip / limited access playtests** |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Scheduled jobs API (named jobs, search, upcoming preview, state history, cancel/modify)** |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| **Surveys / custom signup fields / consent acknowledgements as player metadata** |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/2 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Goal assignment customization via Extend override (e.g. by player level) | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — generic WASM hooks exist but no quest/goal-assignment hook point (rg `quest.*hook` none) |
| Events: rules, rewards, event stores, group events |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Live events (Zanshin/liveops host API) + store windows + audiences; no event-attached sto… |
| Notifications pub/sub (local) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Push: register device (`push/register`), send (`push/send`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Remote configuration pattern |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Global Properties (remote config) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| RTT (realtime channel) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Message input validation with Zod `validate()` — failing message disconnects client with 4002 (or rejects the… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — IDL types validate `/v1/rpc/{name}` input and `[matches.*] params/state`; per-message val… |
| Request/response: handler return value answers `room.request()` / `room.send(type, payload, cb)`; `ctx.reject… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — socket `rpc` envelope `{cid, msg}` gives request/response for RPCs; no reply-to for in-ma… |
| China Gateway |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Operator may host anywhere; no ICP/compliance service |
| NAT P2P Interface: ICE-style connection (local → STUN direct → TURN relay), socket-id keyed connections, acce… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — musubi has no client↔client P2P/NAT traversal or relay servers; relayed matches route thr… |
| Stream sessions |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| TLS for service API |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — HTTPS at edge, API keys/JWT; no FIPS mode (`rg -i fips` → none) |
| Base Classes: name, equippable Contexts, default loadout slots; Default Character Class mandatory; Context se… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Item equip/unequip slots (`/v1/inventory/equip`) and item kinds exist; no class archetype… |
| Connect accounts endpoints for Steam, Xbox, PSN, Nintendo, Google Play Games, Epic, Discord (Unified Player A… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Game Config asset pattern (Generic Simplified context + KV/files/data entities as remote config) |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Hero Classes: specialised sub-class with own loadout and Can Equip / Can't Equip exceptions; in-game create/u… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — rg -i 'hero\|loadout\|character class' = 0 |
| Onboarding record `ll.onboarding.<playtest>` with document content_hash + acknowledgement timestamps (consent… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Age gate / parental consent records exist (`0107_age_gate.sql`); no generic document-ackn… |
| Client integration: `GotLiveOpsEventUpdate` listener, interactive reward claiming (`AllowRemove`, `OnLatestUp… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no client SDK events |
| API keys (client) vs server keys (console API); users management in console; General settings (UTC display) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi keys with roles via `POST /v1/admin/keys`; operator accounts/SSO |
| Audit log with Undo of supported console actions |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi audit log (0026_audit) records; content versions have rollback, generic actions do… |
| Computed properties per event: <event>Count/SeenFirst/SeenLast/ValueFirst/ValueHigh/ValueLast/ValueLow/ValueS… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — no event-derived aggregates per player (stats are explicit server writes) |
| Custom properties registered in Taxonomy with schema; read-only (server/console-only) flag |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — account `metadata` JSON + storage; no taxonomy/schema or per-key client write protection… |
| Data management: import/export via file, import from another Satori server (add server), delete data, sample… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi content bundle export/import between environments (dry_run, prune); fixtures for s… |
| Get returns delta of changed nodes when previous graph passed; multiple local counts per node; programmatic r… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi graphs: claim/skip/unlock/reset per node, rank-based; no delta diff API |
| Privacy: identity deletion (GDPR) purges events/properties/inbox; `LimitedForAnalytics`/`LimitedForPersonaliz… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Tutorial sequences with step tracking |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — rg -i tutorial -> none (can be emulated with storage objects but no system) |
| Ownership & authority (PUN) |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — world entity ownership & handover across cells (`musubi-mesh`); no client-side view owner… |
| Ownership transfer modes |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — world entity ownership handover server-side |
| Premium/dedicated regions on request |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Shared Authority mode |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — relay match state + world entity ownership/handover; no per-object authority request prot… |
| Shared Mode Master Client |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — relay host designation; no unresponsive-host client vote |
| Gateways: HTTP + WebSocket, protobuf or JSON |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Persistent EndGame via SQS |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — end-game reports are persisted in Postgres with idempotent `POST /v1/server/matches/{id}/… |
| `DataRightsServiceConfig` / `DataRightsPollingServiceConfig` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 — synchronous export, no polling job. |
| Actor primitive |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — musubi has `musubi-actor` (authoritative match actors: tick loop, snapshots, journal, fai… |
| Client protocol negotiation (request protocol version) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Custom headers on client requests |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Identity: email verification, access-token identities, friend requests |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Low-level HTTP handler (`onRequest`, WinterTC Request/Response, Hono inside actor) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `POST /v1/rpc/{id}` only; no arbitrary HTTP routing into a module |
| Queue-driven workflows: `queue.next` waits, request/response, drains, signals, human-in-the-loop approvals |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `POST …/{id}/signal {payload}` wakes a parked workflow (README L422); no typed queue inte… |
| Serverless workers invoked by the control plane over HTTP (`/api/rivet/start`), request held open; `requestLi… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — `rg -i serverless` = 0; musubi runs WASM inside its own nodes only |
| FakeIP |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — Steam-relay specific |
| Dead letter queue |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Rate limit |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Schedule configs: one-time, recurring (cron), interval (every N units) with optional firing window |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Cron `schedule_*` module exports (`GET /v1/admin/runtime/schedules`, `/run`) are declared… |
| Settings fetched with `GET/POST /api/v1/settings` client API (attributes in body) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Ownership transfer / `OwnershipRequestStatus`, `IsSessionOwner` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |

### 2.14 moderation/anti-cheat (9 normalized features, 30 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Bans (account, timed, shadow, device/IP/MAC, feature bans, reason catalog, ban sync)** | ✅ ×2 |  | ✅ |  | ✅ |  | ✅ |  | ✅ | ✅ ×3 |  |  |  | ✅ | ✅ | ✅ ×4 | ✅ ×6 |  |  | ✅ ×4 |  |  | 🟡 9/11/5 · P0 |
| **Anti-cheat integration (EAC/BattlEye tickets, protected client, integrity, kick reasons, signals)** |  |  |  |  | ✅ |  | ✅ ×2 |  | ✅ ×2 |  |  | ✅ ×2 |  |  |  |  |  | ✅ |  | ✅ ×2 |  | ✅ | ✅ 7/1/1 |
| **Player reports / incidents workflow (categories, evidence packages, queue, moderator actions)** |  |  |  |  |  |  | ✅ |  |  | ✅ |  | ✅ ×2 |  |  |  | ✅ ×2 |  |  |  |  |  | ✅ ×3 | ❌ 2/1/3 · P0 |
| **Rate limiting / abuse protection / request validation** |  |  | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ | ✅ |  |  |  | ✅ 3/1/1 |
| **Admin moderation tooling (notes, moderator roles per resource, kick, mute, message delete)** | ✅ |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ 2/1/0 |
| **Sanctions service (typed actions, durations, pending/approve, appeals, client-queryable, sync feed)** |  |  |  |  |  |  | ✅ ×4 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 0/4/1 · P0 |
| **Minimum client version / API blocking / compatibility switchboard** |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Text moderation provider (ML/OpenAI/local) for names, chat, UGC** |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 1/0/0 |
| **Voice moderation / voice evidence / child-voice detection** |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Platform security overview |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Argon2/bcrypt pw, JWT rotation, GDPR export/delete routes; backups/VPC are operator respo… |
| Signed (server-to-server) requests |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Token masking in CLI logs (`--unmask-logs`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Secrets handling |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `@colyseus/database` (0.18, Drizzle ORM 1.0-rc): one `GameDatabase` → `db.auth`, `db.saves`, `db.leaderboards… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Org API token (`Authorization: token …`) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Secrets encrypted at rest (secret env vars, S3 secret, registry token) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Secrets live in config/env; `musubi config` prints them |
| AWS managed policies |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| IAM policy examples |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — key roles documented in README |
| IAM troubleshooting / service-with-IAM |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Security overview |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Set up AWS account & role |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — API keys bootstrapped by `musubi config` |
| Server-side impersonation two ways: token exchange from a Game API session token (player online) or by Player… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Security bulletins (MET-2024-001 … MET-2026-002, e.g. SharpCompress CVE-2026-44788) with severity and affecte… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no `SECURITY.md`/advisory process in repo |
| Cheat protection by determinism |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — tick hashes for desync detection (tick_hash 2 files) + authoritative server sim free; no… |
| Checksum interval / desync dump |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — tick hashes compared server-side; no frame dump tooling |
| EntityRef hijacking protection |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Moderation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Inspector token separate from admin token |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Secrets stay in your cloud (BYOC) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Data residency / on-prem |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Outbound HTTP kill switch |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — modules have no outbound HTTP |
| SLA / uptime guarantee |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — self-hosted |
| Sandboxing of app code |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Transport security |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| UGC / moderation / reports |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Secure session tracking |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — match sessions tracked (`match_presence`), but no opaque cheat-data blob ingestion |
| DTLS / TLS everywhere |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Screening results |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ❌ · P1 |
| Unity Moderation Unity package |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — `POST /v1/reports` from clients; no Unity package. |

### 2.15 analytics/observability (10 normalized features, 80 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Backend metrics (Prometheus/OTel) + shipped Grafana dashboards + alert rules** |  | ✅ ×12 | ✅ |  |  |  |  | ✅ ×2 | ✅ ×2 |  | ✅ |  |  | ✅ ×2 | ✅ ×2 | ✅ | ✅ ×4 | ✅ ×4 | ✅ |  | ✅ | ✅ | 🟡 15/9/8 · P0 |
| **Product analytics dashboards (DAU/MAU, retention, funnels, revenue, sessions)** | ✅ ×2 |  | ✅ ×3 | ✅ ×2 |  |  |  |  |  | ✅ ×2 |  | ✅ | ✅ ×3 |  |  | ✅ ×7 |  |  |  |  | ✅ ×5 |  | ❌ 1/8/13 · P0 |
| **Tracing / profiling / per-request tracing propagation** |  | ✅ ×2 |  |  |  | ✅ |  |  | ✅ ×2 |  | ✅ ×5 |  |  | ✅ |  |  | ✅ |  | ✅ |  |  | ✅ | ✅ 10/3/0 |
| **Client telemetry / event ingestion (batched events, schema registry, pre-login keys, sampling)** | ✅ |  | ✅ ×2 |  |  |  |  |  |  |  |  | ✅ ×2 | ✅ ×3 |  |  | ✅ ×3 | ✅ ×2 |  |  |  | ✅ |  | 🟡 3/6/5 · P0 |
| **Analytics export / warehouse connectors (S3, BigQuery, Snowflake, ClickHouse, Kafka, Amplitude)** | ✅ |  | ✅ ×2 |  |  |  |  |  |  |  |  | ✅ | ✅ |  |  |  |  | ✅ |  |  | ✅ |  | 🟡 0/6/0 · P0 |
| **Logs: structured logging, hosted log query, runtime log-level change, per-request dynamic levels** |  |  |  | ✅ |  |  |  |  | ✅ |  | ✅ |  |  |  |  |  |  | ✅ | ✅ ×3 |  |  |  | ✅ 4/3/0 |
| **Status page / health endpoints / SLA metrics** | ✅ |  |  | ✅ |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ | — 0/0/0 |
| **Client incident / crash reports / SDK error reports** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |  |  | ❌ 1/0/1 · P1 |
| **Per-player event history / CS timeline** |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ ×2 |  |  |  |  |  |  | ❌ 0/1/2 · P1 |
| **GeoIP lookup / country enrichment** |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/0/1 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Game telemetry API (5): save events (public, protected/server), admin operations; sessionhistory X-Ray (2) | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Microservice metrics in Portal |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus `/metrics` incl. per-module fuel/time; no UI |
| Telemetry API (`AnalyticsTracker.TrackEvent`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `/v1/analytics/events` batch ingest exists (README analytics row) but forwarded to extern… |
| `platform_ua_attribution` user-acquisition events |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No attribution ingestion |
| App analytics counts via API |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus metrics + Zanshin analytics; no in-API daily/monthly user counts endpoint |
| Hosted Room Servers (Docker) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Logs: Recent Errors, Server (S2S) logs, Unreadable Requests, Job Queue, Security Logs, per-user request logs |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — request recordings/replay (`/v1/admin/dev/recording`), audit log, tracing; no per-user re… |
| Report service (analytics reports via API) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — analytics delegated to Zanshin; no report API |
| SuperRTT (system connections) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — admin session revoke per account; no app-wide disconnect-all |
| Analytics `db.analytics.track(event, userId, props)` to `colyseus_analytics_events`; `AnalyticsPlugin` auto c… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Distributed Relay network |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Musubi relays client-hosted matches through its own cluster nodes (WS/QUIC/WebTransport/W… |
| Logging Interface: categories, levels 0–600 (Off, Fatal, Error, Warning, Info, Verbose, VeryVerbose), callbac… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — server-side tracing (tracing + OTel + trace_id) exists; no client SDK |
| Metrics Interface: BeginPlayerSession/EndPlayerSession (account id type, display name, controller type, serve… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus /metrics (CCU, sessions, matches) + Zanshin analytics events integration + OTe… |
| CloudWatch metrics (fleet/instance) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus `/metrics` + OpenTelemetry for server; fleet gauges (ready/allocated); retenti… |
| CloudWatch metrics (game/player sessions) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — match counts, allocation latency, matchmaker waits exist; no player-session activation /… |
| Console dashboard |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no UI |
| Console dashboards |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — README: admin console UI not written (API only) |
| Console fleets page |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no UI |
| Console queues page |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no UI |
| Fleet debugging guide |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi doctor`, `GET /v1/server/fleets/{id}/servers` states |
| Logging & monitoring overview (security) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Metrics setup / scenarios / troubleshooting |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — README ops docs; no scenario playbooks |
| Monitoring |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Monitoring overview |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus/OTel/audit |
| Troubleshooting index |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Download full logs for stopped process |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — as above |
| Log/metrics retention |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `[retention]` days per table for events/audit/chat; no process-log retention because logs… |
| Per-process logs stream |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg stdout/log_stream` in musubi-fleet → no capture of game-server stdout/stderr; relies… |
| Process search by roomId / processId / external id, filter region/status/created-before |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/matches?label=…`, admin cluster matches; no unified process search with time fil… |
| User roles (RBAC) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Health/status API *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Databricks churn-prediction loop: export Parquet, run ML, write `dbChurnPrediction` custom property back via… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — none |
| Identify (anonymous -> permanent identity merge with property/event precedence rules); server events API for… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi account merge (0061) merges accounts/wallets/inventory; no analytics identity merge |
| Invalid event debugger: INVALID_NAME/VALUE/METADATA, VALUE_LENGTH_EXCEEDED, METADATA_SIZE_EXCEEDED, INVALID_ID |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | — — no client analytics ingestion |
| Publisher interface (server-side event listeners) |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ |
| Server-Event API: name, optional UUID id (dedupe), metadata, value, timestamp, identity_id/session_id optiona… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi event bus accepts module/server-emitted events; Zanshin forwarding; no analytics s… |
| Sessions: `session_token_expiry_sec` (1.5 d default) / `session_refresh_token_expiry_sec` (7 d); client-, ser… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi session TTLs configurable (`[session]`), no playtime analytics |
| Analyzing disconnects / performance tests |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Chat counters |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus chat metrics; no dashboard |
| Counters per app |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus/OTel; no per-app multi-tenant counters (musubi is single-tenant) |
| Performance tests & disconnect analysis |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Economy events: player_vc_item_purchased, player_realmoney_purchase, player_receipt_validation, player_consum… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Insights performance levels & retention: GetDetails/GetLimits, SetPerformance (levels 1–5+), SetStorageRetent… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Managed warehouse tiering |
| Legacy event archiving to S3 (≤3 archives, 250 event names) and EventArchive settings |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Legacy |
| MPS/Party/Lobby/Matchmaking PlayStream events: matchmaking_match_found / ticket_completed / user_ticket_compl… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — Fleet & match events on event bus (server allocated/ready/stopped, match created/ended, m… |
| MPS: LinuxInstrumentationConfiguration (VM metrics for Linux), new VM families HBv3/Ddv5/Ddsv5/Dadsv5, Sweden… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Azure-specific |
| Manage Events: per-event sampling ratio 0–100% (takes 4h), Event Partitioning (private preview, ≤20 events →… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No per-event sampling config (grep sampling: tracing only) |
| PlayStream built-in event catalog (~157 event pages): player_* (logged_in, added_title, banned, linked/unlink… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — README event bus: typed server events for account/auth/economy/match/fleet/moderation (mi… |
| Sessions: client_session_start / client_focus_change events auto-emitted by SDKs ≥180809 for screentime |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No client SDK → no automatic session/focus telemetry |
| Title/studio events: title_api_settings_changed, title_added_cloudscript, title_created/aborted/completed_tas… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | 🟡 · P2 — Audit log captures admin config changes; no studio/tenant events, no rate-limit alert eve… |
| Webhooks (PlayStream): POST events to external URL, batch as JSON array option, ≤3 custom headers (64/256 cha… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Dedicated telemetry backend |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 |
| Dependency: MySQL only, Liquibase migrations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — musubi uses Postgres (122 sqlx migrations). Equivalent, not a gap. |
| Engine metrics catalogue |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Logging |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `tracing` JSON logs with `RUST_LOG`; no runtime level change or remote log fetch. |
| Sampling |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 |
| Session-service broadcast batching |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 |
| Storage plugins |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 — no pluggable event sink besides webhooks/Zanshin. |
| `TelemetryServiceConfig` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 |
| Actor Inspector (local dev button, per-actor inspector token, custom inspector tabs shipped with the actor) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — no inspector (`rg -i inspector` = 0) |
| Debugging guide: list actors by name/key/id, read KV, inspect runner connectivity, pool errors |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `GET /v1/server/matches`, `/worlds`, `/gameservers`, `/cluster/nodes`; no written runbook |
| Error forwarding to Sentry from workflow `onError` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — any Rust `tracing` sink works |
| Live chat / feedback widget in Engine & Inspector; PostHog in Sandbox Agent |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — |
| Live latency shown per runner in dashboard list |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — node heartbeats in `GET /v1/server/cluster/nodes`; no latency UI |
| Page-cache / VFS metrics & tuning |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — no actor-local DB |
| `rivet logs` CLI (follow, filter by actor/name/text, JSON, pool) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — no log CLI (`musubi-cli` has none) |
| Dashboard metrics |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `GET /metrics` Prometheus (27 files), pprof/flamegraph (`/v1/admin/debug/profile`); no bu… |
| Full row history retention |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — musubi has audit log (`0073 audit`) and match/world journals; no general row-history for… |
| SQL console in dashboard |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — no admin console (parity.md) — use psql |
| Attribution / MMP acquisition-source events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ❌ · P1 — No attribution fields; out of scope of core |
| Consent management (opt-in/opt-out, `EndUserConsent` API, PIPL/GDPR regions) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Age gate / parental consent (`/v1/account/parental/request`, COPPA) and GDPR export/delet… |
| Custom user id support |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Data deletion request (player) via REST |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Debugging tools (web proxy, SDK debug panel) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — |
| Environment support, user roles for analytics |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Server-key roles (support/liveops/ops); no analytics-specific roles |
| Event Browser (debug) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — `GET /v1/admin/audit` + webhook delivery logs; no event tail UI |
| Game data deletion (whole project) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Drop your DB |
| REST Collect API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Server-side event ingestion only (`/v1/server/events`, server key); no client-authenticat… |
| Standard events auto-collected by SDK (gameStarted, gameRunning every 60 s, transactions, ad impressions, acq… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Server emits `musubi.events` bus (`0027_events.sql`) for backend facts (login, purchase,… |
| Metrics |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Server metrics via Prometheus (`musubi_*`). |

### 2.16 runtime/extensibility (10 normalized features, 114 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Server-side scripting runtimes (WASM, JS/TS, Lua, C#/.NET, Go, Kotlin, Python)** | ✅ ×5 |  | ✅ ×22 | ✅ ×4 |  |  |  |  |  |  |  | ✅ ×3 |  | ✅ ×6 |  | ✅ | ✅ | ✅ ×2 | ✅ ×5 |  | ✅ ×4 |  | 🟡 15/22/5 · P0 |
| **Public API surface & conventions (OpenAPI, gRPC transport, error format, rate-limit headers, deprecation policy, compression)** | ✅ |  | ✅ ×2 |  |  | ✅ ×2 |  |  | ✅ |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |  | ✅ 6/2/0 |
| **Webhooks (outbound events, signatures, retries, deliveries log, redeliver, test)** |  |  | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ ×3 | ✅ |  |  |  |  |  |  | ✅ 5/0/1 |
| **Custom HTTP routes / handlers from module code (+ middleware, CORS, basic auth)** |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |  |  | ✅ |  | 🟡 0/3/0 · P1 |
| **MCP server / AI agent skills / LLM-friendly docs** | ✅ |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 0/1/0 · P1 |
| **Outbound HTTP / external calls from module code (with limits, kill switch)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |  | ✅ |  |  |  | ✅ 2/1/0 |
| **Module deployment: hot reload, versions, canary, rollback, env vars, local debugger, templates/unit-test scaffolds** |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |  |  |  | 🟡 1/1/0 · P1 |
| **Federation hooks (external inventory/NFT, external player init, challenge/solution wallet auth)** |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 1/0/1 · P2 |
| **Generic before/after hooks on every API & realtime message, disable API by hook** |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 0/1/0 · P2 |
| **Typed RPC with generated client stubs and typed application errors** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 0/1/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| CSM API (107): app lifecycle V1–V5 (create, deploy, start/stop, status progress), images, configurations & en… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — modules deploy/canary/promote/rollback + status via CLI/admin API; no managed DB provisio… |
| Extend Event Handler: Kafka Connect sidecar → gRPC on AGS events (proto descriptors), idempotency guidance, m… | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Login allowlist: only accounts holding allowed roles can log in (closed tests/events) | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no role-based login allowlist (rg `allowlist` none) |
| Account PII & admin endpoints |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Deployment plans (`deploy plan` / `deploy release`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi deploy` manifest + module publish/canary/promote/rollback; no diff/plan preview s… |
| Enable/disable deployed services |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — module unpublish/drop; no per-environment enabled flag |
| Events calendar & phases API |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Live events + tournament windows; no phased event object |
| Game relay (`/object/gamerelay`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Group users: recommended groups, search, petition/apply, invite, kick, donations |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Search/apply/invite/kick exist; no recommended-groups endpoint or donation ledger |
| Idem multiplayer matchmaking API |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi has native MMR matchmaking |
| Inventory transfer / preview / multipliers |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — P2P trades + server tx exist; no dry-run preview endpoint, no multipliers (reward modifie… |
| Leaderboard UID & assignment |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Legacy systems still exposed |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Coupons/entitlements/timers exist; no shard-assignment legacy API (worlds/cells differ) |
| Mail bulk + templates |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Notification channels |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Partial-class code organization / shared Common assembly / `[Agnostic]` content |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — C#-specific |
| Realms API |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Projects/environments + config; no promotion/launch-message/plans |
| Session service |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Stats search & subscribe |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Stats query exists per account; cross-player search by stat criteria only via audiences/l… |
| Tournaments admin/champions/global/standings |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `[ServerCallable]` scheduler-only methods & signed-request auth |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| API Blocking |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — per-key role scoping and rate limits (`limits.rs`); no per-operation client blocklist |
| API hooks (pre/post, postFail) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Batch user scripts |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — audience-targeted mail/push; no “run script per user over a segment with completion callb… |
| Bridge API |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| HTTP Client (outbound) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| S2S scripts & S2S API |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Scheduled scripts |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Script usage / profiling |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Advanced: custom primitive types (varInt/varUint/varBigInt/varFloat32/varFloat64), byte-level encode/decode h… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Rust-side encoding is fixed (serde/postcard); memory pooling is inside the WASM module |
| Message handling: declarative `messages = {type: handler}` map, `"*"` fallback, runtime `this.onMessage()` re… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `match_input` / `match_data_send` are opaque payloads routed to one handler (`match_<modu… |
| Deprecated endpoints documented |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — No deprecation policy documented |
| Nakama integration |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi replaces Nakama; `migrate-nakama` importer |
| PlayFab bridge |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — PlayFab data importer only |
| REST API v1 |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| REST API v2 |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Anti-Cheat Web API: GET /anticheat/v1/{deployment}/status (service status for deployment) |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| EOS Connect Web API: POST /auth/v1/oauth/token issuing client access tokens (product-level) and user access t… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Ecom Web API v3/v4: ownership check, entitlements list/redeem, ownership & entitlement verification tokens (R… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi consumes Epic entitlements via s2s (/v1/economy/entitlements/sync, Epic provider)… |
| Epic Account Services Auth Web API: OAuth 2.0 token endpoint (authorization code / exchange code / refresh to… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Friends Web API: GET friends list and block list for an authenticated EAS player |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player Reports Web API (send/find reports server-side), Stats/Leaderboards/Achievements Web APIs (ingest/read… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Sanctions Web API: active sanctions per PUID, /sanctions/v1/sync event feed, deployment-wide list/create/patc… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — admin ban endpoints + audit log + event bus; no sync feed/appeals |
| Voice Web API: POST /rtc/v1/{deployment}/room/{room} create room tokens (per PUID) returning media server end… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — /v1/voice/token mints Aurix tokens; no media server, kick/mute passthrough only if provid… |
| Alias object & console |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg -i alias crates/musubi-fleet` → none |
| Cloud game streaming (pixel streaming) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi is a game backend; no GPU streaming (`rg -i webrtc` → realtime transport only) |
| Common API limits / Regions limits |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Encryption at rest |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Postgres at rest is operator's responsibility; secrets encrypted in `musubi-storage`? not… |
| Fleet aliases |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — pools decouple clients from fleets; builds promote within a fleet; no alias object / term… |
| Realtime Servers scripts |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi `musubi-runtime` WASM match modules (authoritative actors, JS/TS via `musubi modul… |
| Realtime client SDK |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi has WS/QUIC/WebTransport/WebRTC/UDP transports but no shipped client SDK (Unreal s… |
| Resource throttling |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator-limited |
| Service API rate limits |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Smithy RPC v2 CBOR protocol |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi uses JSON REST + binary realtime frames (lz4) |
| Cheat-proof randomization: server-decided seeds, commit/reveal patterns, server-side validation of random out… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Custom game server connection (implementing own transport/`IMessageTransport`) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — |
| MetaRewards: `MetaReward`/`MetaPlayerReward` base classes with `Consume(model)`, custom parsers from config s… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Background jobs guidance: JIT per-player processing instead of cron sweeps (no built-in scheduler/cron jobs b… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Client `event` API (name + properties map, timestamp, external=true); runtime `RegisterEvent`, `RegisterEvent… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Dev tooling: Jest testing guide for TS modules, Delve debugging via `nakama-dsym` image, Go dependency pinnin… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi dev` local loop, fixtures, request record/replay, bench/soak (README dev rows); n… |
| Email: no built-in SMTP; SendGrid via `httpRequest` guide |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Error reference: gRPC codes↔HTTP mapping, predefined runtime errors (`ErrStorageRejectedVersion`, `ErrMatchNo… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Fetch users by IDs/usernames (client and runtime); update own `username/displayName/avatarUrl/location/langTa… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Per-connection status string/JSON; appear offline by clearing; follow/unfollow users; event with old+new stat… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Performance best-practices doc (≤5 DB round trips per handler, zero per match tick, batching) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | — — guidance, not a feature |
| Permissions: read 0/1/2 (none/owner/public), write 0/1; client default owner/owner, runtime default none/none… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| RPC functions (`RegisterRpc`) callable by clients (session) or server-to-server (`runtime.http_key` via query… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Rate limiting / quotas per API — none built-in (hooks + external) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| User metadata JSON ≤16 KB, writable only from server runtime |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| `RegisterMatchmakerMatched` (create authoritative match for matched users or return match id), `RegisterMatch… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — `hook_matchmaker_matched` creates match / allocates fleet server; no override/processor |
| Addon API (Admin): CreateOrUpdate/Delete/Get for Apple, Facebook, FacebookInstantGames, Google, Kongregate, N… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Insights API (legacy): GetDetails, GetLimits, GetOperationStatus, GetPendingOperations, SetPerformance, SetSt… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Managed-analytics tier controls |
| Sweepstakes API (preview, Xbox): CreateSweepstake/EnterSweepstake/GetSweepstakeEntries/Winners — raffles/give… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ❌ · P1 — No sweepstakes/raffle primitive (reward tables could emulate) |
| Broadcast notifications |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — notifications to accounts/audiences/all (`POST /v1/server/notifications` with audience `a… |
| Concurrency utilities |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Custom errors (application vs service) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — module RPCs return `{code, message, details}` JSON errors (RFC-7807 style); not typed in… |
| Custom persistence (DAO node services) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — modules use storage collections (JSON, indexes, contracts) — no raw SQL tables per module. |
| Custom services (`DistributedService`, `@PragmaService`, `@PragmaRPC`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — WASM module RPCs (`POST /v1/rpc/{id}`, socket `rpc`) with IDL; no generated typed client… |
| Login data aggregation plugin |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 — no single aggregated login payload hook. |
| Plugins (`@PragmaPlugin`, `ConfigurablePlugin`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — WASM hooks on ~40 events + webhooks; cannot replace core algorithms (e.g. matchmaker loop… |
| Durable timers `ctx.sleep` / `sleepUntil`, `ctx.loop` durable loops, joins, races, fan-out |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `sleep_secs` and `wait_signal` parking (README L418, `workflows.rs`); no join/race/fan-ou… |
| Reschedule actor to another runner (`POST /actors/{id}/reschedule`), force sleep (`POST /actors/{id}/sleep`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `POST /v1/server/matches/{id}/migrate` / mesh handover; no force-sleep |
| Schedule history (`maxHistory` 100) and `c.cron.list/delete` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — server cron jobs are config-defined (`schedules.rs`), no runtime list/history API |
| Worker health route (200/503) and metadata route (runtime version, actor names) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| `c.schedule.after(ms, action, args)` / `schedule.at` durable one-shot alarms that survive sleep/restart/upgra… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — server-side `musubi-server/src/schedules.rs` + workflows `delay_secs`/`sleep_secs`; no pe… |
| `onStateChange` hook |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `storage_subscribe` → `storage_update` notifies clients of storage changes; no in-actor h… |
| Automatic (compatible) migrations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `musubi migrate` runs fixed SQL migrations for the backend's own schema; module state sch… |
| Type system (SATS/BSATN) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — IDL + JSON/binary realtime encoding; `GET /openapi.json` for REST; no equivalent self-des… |
| Machine-readable API list |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| REST layout `/<interface>/<method>/v<N>/` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Response formats json/xml/vdf |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — JSON only (plus postcard binary on the socket); no XML/VDF — irrelevant for most studios |
| Status codes & rate limiting |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| User (non-publisher) API keys |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — not applicable to a self-hosted backend (player bearer tokens cover the same) |
| Versioned methods (v1/v2…) kept backward compatible |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — single `/v1` namespace; no documented multi-version policy yet (alpha) |
| Access tokens vs service tokens inside code to call other UGS services |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Admin API (CRUD scripts/modules, publish, module spec) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Batch requests from one module function |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| CI/CD + config-as-code deploy (UGS CLI `ugs deploy`, Deployment window) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Call external HTTP APIs from server code |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Canary / percentage rollout of server code |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Client invocation API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Context object (playerId, projectId, environmentId, accessToken, serviceToken) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Cost model |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Self-hosted compute; fuel accounting per RPC exposed in metrics |
| Game-state / cheat-prevention guidance (server authority) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| In-memory cache between invocations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Logging + log query (Observability API, filter language) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Module `log` host op → structured server logs + OpenTelemetry traces (`otel` in 3 crates)… |
| Module metadata tags |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Module versions carry name/version/notes; no free-form tags |
| Push messages to connected players (WebSocket, player/project subscription tokens) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Scheduled start/end of an override |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Live events (`/v1/liveops/live-events` via Zanshin) and store listing availability window… |
| Script versioning, publish vs draft, test-run previous versions |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Module versions + rollback exist; no "draft/unpublished" state or dashboard test runner (… |
| Secrets inside server code |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Trusted-client (server) calls with Basic auth / service account |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Unit-test project templates (Rider/VS) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — `musubi-sim-sdk` + fixtures/replay for Rust modules; no scaffolding command for module te… |
| Warm-up endpoint (`POST /projects/{P}/warm`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — No cold starts in musubi's resident WASM runtime |

### 2.17 SDKs (14 normalized features, 134 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Unity / C# client SDK (typed REST + realtime client, session refresh, offline cache, UPM package)** | ✅ ×3 | ✅ ×4 | ✅ ×12 | ✅ ×3 | ✅ ×3 | ✅ ×4 | ✅ ×2 | ✅ ×7 | ✅ ×5 | ✅ ×2 |  | ✅ ×4 | ✅ ×3 | ✅ ×4 | ✅ ×5 | ✅ ×4 | ✅ ×3 | ✅ | ✅ ×4 |  | ✅ ×3 | ✅ ×4 | ❌ 6/18/45 · P0 |
| **Unreal client SDK / Online Subsystem plugin (finished, CI-built)** |  | ✅ | ✅ ×5 | ✅ |  | ✅ ×2 | ✅ | ✅ ×3 | ✅ ×2 | ✅ ×2 | ✅ |  |  |  | ✅ | ✅ ×2 | ✅ ×3 |  | ✅ ×2 |  | ✅ ×2 | ✅ | 🟡 1/22/3 · P0 |
| **Typed client code generation from schema/IDL (models, collections, RPC stubs)** |  |  |  |  | ✅ ×2 |  |  |  | ✅ ×2 |  | ✅ | ✅ |  | ✅ | ✅ | ✅ |  | ✅ |  |  | ✅ |  | ✅ 6/3/2 |
| **Native C/C++ client SDK (consoles, custom engines)** |  | ✅ |  |  | ✅ ×3 |  | ✅ ×3 |  |  |  | ✅ ×2 | ✅ |  |  | ✅ | ✅ ×3 |  |  | ✅ |  |  |  | ❌ 2/4/8 · P0 |
| **SDK migration tools from other backends (PlayFab/UGS/Nakama importers)** |  |  | ✅ | ✅ |  | ✅ |  |  |  |  |  |  | ✅ |  |  |  | ✅ |  | ✅ | ✅ | ✅ |  | ✅ 6/1/0 |
| **Samples, templates, tutorials, starter projects** |  | ✅ |  |  | ✅ |  | ✅ ×3 |  | ✅ ×2 |  |  | ✅ ×3 |  |  | ✅ |  | ✅ | ✅ ×2 |  |  |  |  | ✅ 5/3/1 |
| **TypeScript / JavaScript / Web SDK (+ React hooks, Node server SDK)** |  | ✅ | ✅ ×3 |  | ✅ ×4 |  |  | ✅ ×3 | ✅ |  |  | ✅ |  |  |  |  |  | ✅ ×4 | ✅ |  |  |  | ❌ 4/3/6 · P0 |
| **Godot SDK / GDScript addon** |  | ✅ |  |  | ✅ | ✅ |  |  | ✅ | ✅ |  |  |  |  | ✅ ×2 |  |  |  | ✅ |  |  |  | ❌ 1/0/7 · P0 |
| **SDK ergonomics: auto-reconnect/backoff, request bundling, compression, RTT/jitter sliders, error reports** | ✅ |  |  | ✅ ×2 | ✅ ×3 |  |  | ✅ |  |  |  | ✅ |  |  |  |  | ✅ | ✅ ×3 |  |  |  |  | 🟡 4/6/2 · P0 |
| **Other language SDKs (Swift, Java/Kotlin, Dart/Flutter, Python, Go, Rust, Lua, Defold, Cocos, Haxe, GameMaker, Construct, Roblox, Discord Activities, WeChat)** |  | ✅ ×3 |  |  |  |  |  |  | ✅ ×2 | ✅ |  | ✅ ×2 |  |  |  |  | ✅ ×2 | ✅ ×2 |  |  |  |  | ❌ 1/4/7 · P0 |
| **Load testing / bench tools for SDK users** |  |  | ✅ |  | ✅ ×2 |  |  |  |  |  |  | ✅ | ✅ |  |  | ✅ |  |  |  |  |  |  | ✅ 3/1/0 |
| **Console SDKs (PlayStation, Xbox GDK, Nintendo Switch) and certification notes** |  |  |  |  |  |  | ✅ ×2 |  |  |  | ✅ |  |  |  | ✅ ×3 | ✅ ×2 |  |  |  |  |  |  | ❌ 0/2/3 · P1 |
| **Editor tooling (deployment window, in-editor config authoring, content sync, inspector, playground)** |  |  |  |  | ✅ ×2 |  |  |  |  | ✅ |  | ✅ |  |  |  |  |  |  |  |  | ✅ |  | 🟡 2/2/0 · P1 |
| **Server-side SDKs for game servers (Unreal DS, Unity server, C++, C#, Go, Rust, Node, Python)** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 0/1/0 · P0 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Alpha player tracking `PlayerConnect/Disconnect`, `SetPlayerCapacity`, `GetPlayerCapacity`, `GetPlayerCount`,… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Beta counters: `GetCounterCount`, `SetCounterCount`, `IncrementCounter`, `DecrementCounter`, `SetCounterCapac… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Only `POST /v1/gameserver/players {players, capacity}` |
| Beta lists: `GetList`, `UpdateList`, `AddListValue`, `RemoveListValue` (+ language helpers `AppendListValue`,… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Connecting via env `AGONES_SDK_GRPC_PORT` / `AGONES_SDK_HTTP_PORT` auto-discovered by SDKs |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Local SDK server binary (`sdk-server --local`, `--file gameserver.yaml` mock, `--feature-gates`, `--sdk-name`… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi dev` runs the whole backend locally (README:operating commands); no standalone mo… |
| SDK conformance tests per language (`test/sdk/*`) run in CI |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — CI runs the server suite against Postgres (README:status); no SDK conformance matrix |
| SDK sidecar request rate limiter (`agones.sdkServer.requestsRateLimit`), constant since 1.53 |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| State-changing SDK calls are asynchronous and batched; callers must confirm via `WatchGameServer` |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Swagger/OpenAPI definitions of the SDK REST API (`sdks/swagger`) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `Allocate()` — self-allocate |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `GameServer()` — returns ObjectMeta, Spec (health, ports), Status (state, address, addresses, ports, players,… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `Health()` client-streaming ping |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `Ready()` — move to Ready, also returns an Allocated server to Ready |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `Reserve(seconds)` — `Reserved` state: not deletable on scale-down/update, not allocatable, auto back to Read… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No reserved state (`rg -i reserve crates/musubi-fleet crates/musubi-server/src/http/fleet… |
| `SetLabel(k,v)` / `SetAnnotation(k,v)` with forced `agones.dev/sdk-` prefix (isolation / attack surface) |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/gameserver/label` replaces the whole labels JSON; no annotation channel, no pre… |
| `Shutdown()` — Shutdown state, Pod terminated; SDK server stays alive through `terminationGracePeriodSeconds`… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `WatchGameServer(cb)` — server-streaming callback on every change (state, labels, annotations, allocation met… |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/gameserver/watch?since=<version>&timeout_secs` long-poll on `version` counter (m… |
| Offline mode matrix |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No client cache layer |
| Rust autogen client (`beam_autogen_rs`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Server is Rust; clients via OpenAPI |
| Wrapper persistence |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no SDK |
| Client join methods `joinOrCreate`, `create`, `join`, `joinById` (private rooms joinable by id → invite links… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — HTTP/WS API: `POST /v1/matches` (create), join by id, invite codes, lobbies, matchmaker t… |
| Latency & multi-region: `client.ping({pingCount})` before join, `room.ping()` during, `Client.selectBestEndpo… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/regions` + ping endpoints per region and server-side region scoring in matchmake… |
| Messaging: `room.send(type, payload)`, `sendBytes(Uint8Array)`, `onMessage`, request/response `room.request(t… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `MatchData`/`Rpc` realtime messages with request ids; no SDK helpers |
| Engine-agnostic server ("no server SDK required") |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Dependencies/network: hosts api.epicgames.dev, overlay CDN, *.live.rtcp.on.epicgames.com (voice), ecom hosts;… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted: operator controls endpoints |
| EOS SDK in C (stable ABI) + C# wrapper; async callback model w/ Tick; custom memory delegates; API version ma… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — musubi has no shipped client SDK (README: "client SDKs ... not written yet"); only OpenAP… |
| Sandboxes & Deployments (dev/staging/live data isolation; deployment id in tokens; per-sandbox identity provi… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| ActivateGameSession / GetGameSessionId / GetTerminationTime |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — allocation implies activation; no termination-time API (no Spot) |
| Backend service SDK (AWS SDK) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Fleet IAM role for game servers |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no credential vending to game servers (server key only) |
| Fleet info from environment |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| GetComputeCertificate / GetFleetRoleCredentials / ListContainersNetworkInfo |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Health check callback |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| InitMetrics / InitCustomLogger / Destroy |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Match backfill from server |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `backfill` flag on allocation & queue `backfill_ttl`; no per-session backfill ticket API… |
| Player session validation |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| SDK data types |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — JSON bodies documented in README/OpenAPI (`musubi idl`) |
| SDK interactions diagram |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| SDK reference index (client/server/plugins/downloads) |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi idl` codegen |
| Server SDK C# reference |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Server SDK Go reference |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| BuildV1/V2 per-app builds |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| CreateApp / UpdateApp / DeleteApp |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — configuration in TOML (`musubi config`) |
| CreateBuild |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no managed builds |
| CreateBuildRegistry |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| CreateFleet / UpdateFleet |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/server/fleets` with ScalePolicy; no node shapes |
| CreateLobby / ListActivePublicLobbies / GetLobbyInfoByRoomId / GetLobbyInfoByShortCode |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| CreateRoom / GetRoomInfo / GetActiveRoomsForProcess / GetInactiveRoomsForProcess / DestroyRoom / SuspendRoom… |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — matches + gameservers endpoints |
| DeleteBuild |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| DeploymentV1/V2 CPU & memory ranges |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `vcpu` for accounting only, no enforcement |
| GetApps / GetApp |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no multi-app object |
| GetBalance / GetUpcomingInvoiceItems / GetUpcomingInvoiceTotal / GetPaymentMethod / InitStripeCustomerPortalU… |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted |
| GetBuilds / GetBuild |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET …/fleets/{id}/builds` per fleet (version, state, canary_percent/regions, notes); no… |
| GetDeployments / GetLatestDeployment / GetDeployment / CreateDeployment |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — fleet spec/build endpoints; see Deployments rows |
| GetFleets / GetFleetRegion / UpdateFleetRegion / GetFleetMetrics |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET/POST /v1/server/fleets`, `POST …/scale`, `GET …/usage`, `GET …/demand` |
| GetLatestProcesses / GetProcessesCountExperimental / CreateProcess / GetProcess / StopProcess / GetProcessMet… |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — fleet servers endpoints |
| GetLogsForProcess / DownloadLogForProcess |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| GetMetrics (legacy) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — see Processes metrics |
| GetNodes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `/v1/admin/cluster/*` for musubi nodes only |
| GetOrgTokens / CreateOrgToken / RevokeOrgToken |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| GetOrgs / GetOrgMembers / GetOrgPendingInvites / GetUserPendingInvites / InviteUser / RescindInvite / AcceptI… |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no team/invite model (`rg invite` → social only) |
| GetPingServiceEndpoints |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| LoginAnonymous / LoginNickname / LoginGoogle |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Org limits object |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — single-tenant self-hosted |
| Per-process metrics |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `/metrics` Prometheus covers musubi nodes, matches, transports; fleet usage is vCPU-hours… |
| RoomV1 / LobbyV1 / LobbyV2 kept as deprecated |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| RunBuild |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Scoped org tokens |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — roles are coarse bundles (5 roles), not per-resource read vs read-write scopes |
| SendVerificationEmail |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Third-party matchmaking integration (Idem) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `ExposedPort{host, port, name, transportType}` |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — address/port only; no named ports/transport attribute |
| `Region` enum fixed to 14 values |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `RoomAllocation` history |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — match history tables (migrations history) for in-process matches; dedicated-server alloca… |
| `hathoraDevToken` vs `playerAuth` security schemes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| API keys per environment (`dev_`/`prod_` prefixes), Domain Key per game, legacy keys flagged in editor |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| SDK multi-user state: multiple concurrent local players, profile switching, active/inactive sessions, clear s… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | — — Client SDK concern; musubi has no client SDKs (Unreal scaffold only) — flagged as SDK gap… |
| Authority handover timing *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Cadence *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — |
| Client authentication *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Commercial model *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — musubi Apache-2.0, any cloud (Terraform k8s/Hetzner/OVH) |
| Gated SDK *(Hadean)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — public GitHub |
| Maintenance & recovery *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Platform SDK *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Release policy *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — alpha |
| Rooms vs Worlds *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Service accounts *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Snapshot upload local→cloud *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Transport *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Changing player name with server validation/profanity hook; dashboard rename |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client build version (`BuildVersion{Version, BuildNumber, CommitId}`) attached to incident reports and dev co… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — client version header checked against min version; not surfaced in reports |
| Client-server request/response (`MetaRequest`/`SendRequestAsync`, `SessionActor` routing, no built-in timeout) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Dashboard testing: 100+ built-in Playwright e2e tests, Vitest unit tests, ESLint/TS, Storybook, fixtures (`fe… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — no dashboard |
| In-game player account deletion request flow (schedules deletion server-side) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player incident reports: client auto-captures crashes/connection failures/unhandled exceptions with logs, upl… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no client incident ingestion endpoint (`rg -i incident` → none) |
| Protobuf or JSON socket encoding (`format=protobuf`), `lang` param, `status=true/false` on connect (appear on… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — JSON + binary framing over WS/QUIC with LZ4; protobuf wire format not offered (musubi-pro… |
| REST + gRPC API surface: 88 RPCs (`apigrpc.proto`) incl. `Healthcheck`, `Event`, `GetUsers`, `GetMatchmakerSt… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Realtime protocol (`realtime.proto` Envelope, ~48 messages): channel join/leave/message send/update/remove, m… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Async/await extensions |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — — no C# SDK |
| Bot SDK |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — matchmaker backfill + "bot" mentions (35 files, mostly chat/bots filler?) — no AI authori… |
| Custom Plugin SDK |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Hive plugin API 1.5 |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Modular plugins |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Network simulation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Plugin versioning & upload |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Raw messages |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Serialization protocol |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Postman collection for all REST APIs; PowerShell MPS module; SDK Generator open-source (apispec-driven) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Client cache & entity lookups |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — storage subscriptions push changes over WS (`StorageSubscribe`), but the Unreal SDK offer… |
| Client-version enforcement events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `Status`/`Error` with min-version code exists on connect; no party-level mismatch event. |
| Friend / block events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `Notification` kinds (`friend.request`, `friend.accepted`, `friend.removed`) + presence;… |
| Game instance events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Game-server (partner) events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Game-server integration (Partner SDK over WS) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Initialize multiplayer services (SDK) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — Unreal plugin exposes raw WS messages + helpers, not a facade with full event set. |
| Matchmaking events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Party client cache + ForceSync |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Party events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — realtime `PartyState` full-state push covers the same changes, but the Unreal plugin expo… |
| Party invite events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `PartyInvite` notification + accept/decline messages; no revoke/cancel notifications to t… |
| Postman collection / dev tools |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Presence events |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| Retrieve data for game servers |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| SDK configuration |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — Unreal plugin settings (`UMusubiSettings`: base URL, ws URL, server key) — fewer runtime… |
| SDK debug logging |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `LogMusubi` category; no detailed-messages toggle documented. |
| Effect.ts SDK (beta) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — ecosystem-specific |
| React `@rivetkit/react`: `useActor`, `useActorEvent`, Suspense, SSR |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 |
| Connection lifecycle callbacks |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `Session`, `Error` server messages; client side absent |
| Procedure/view calls from client |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `Rpc` returns payload; no views |
| Row/table callbacks |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 |
| Rust client SDK |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — no Rust client SDK (sim-sdk is server-side) |
| Cloud Save Files (large binary save files, any format) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Blob storage exists only for chat attachments (10 MiB) and UGC (`[ugc.s3]`, ≤16 MiB, `/v1… |
| Client-side latency/packet-loss measurement SDK |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — UDP echo probe protocol exists; measuring is left to the client (no C#/C++ QoS SDK), pack… |
| REST/OpenAPI coverage |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |
| Relay vs Lobby guidance, REST + SDK (`AllocationUtils.ToRelayServerData`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| UGS CLI |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |

### 2.18 deployment/ops (11 normalized features, 163 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Support / SLA / professional services** | ✅ |  | ✅ ×2 | ✅ |  | ✅ | ✅ |  | ✅ |  | ✅ ×2 |  | ✅ ×2 | ✅ ×4 | ✅ ×2 | ✅ | ✅ |  |  | ✅ | ✅ | ✅ | ✅ 3/0/0 |
| **CLI (dev loop, deploy, logs, doctor, config, module/fleet management, troubleshooting)** | ✅ ×2 |  | ✅ ×9 | ✅ ×2 |  |  |  |  | ✅ ×2 |  | ✅ ×2 | ✅ ×3 |  |  |  |  | ✅ | ✅ ×2 | ✅ |  | ✅ ×3 |  | 🟡 8/13/2 · P0 |
| **Managed cloud / hosted offering, pricing, free tier, regions** |  |  |  | ✅ | ✅ ×2 | ✅ |  |  |  |  | ✅ | ✅ | ✅ ×3 | ✅ ×5 |  |  |  | ✅ ×5 |  | ✅ |  | ✅ ×2 | ❌ 3/4/5 · P2 |
| **Self-hosting packaging (single binary, Docker image, docker-compose, Helm chart, Terraform, PaaS one-click)** |  | ✅ ×3 | ✅ |  | ✅ ×3 | ✅ |  |  |  |  |  | ✅ ×5 |  | ✅ ×2 |  |  |  | ✅ ×9 | ✅ ×3 | ✅ |  |  | 🟡 10/11/3 · P0 |
| **Data residency / regions for control plane / China gateway** |  |  |  |  |  |  | ✅ | ✅ ×4 |  |  | ✅ ×2 |  |  |  | ✅ | ✅ |  | ✅ |  | ✅ | ✅ |  | ✅ 5/3/1 |
| **Upgrade / compatibility runbook, feature gates, breaking-change changelog, versioning policy** | ✅ |  |  |  |  | ✅ |  |  | ✅ | ✅ ×2 |  |  |  |  | ✅ |  | ✅ ×2 | ✅ |  |  |  |  | ✅ 3/2/0 |
| **Multi-environment / namespaces (dev/stage/prod, preview per PR, realm archive, promotion)** | ✅ ×2 |  |  |  | ✅ |  |  |  | ✅ |  |  |  |  |  |  | ✅ |  | ✅ ×9 |  |  |  | ✅ | 🟡 4/7/3 · P0 |
| **Serverless / edge worker runtime mode** | ✅ |  |  |  | ✅ | ✅ |  | ✅ |  |  |  | ✅ |  |  |  |  |  | ✅ ×4 |  |  |  |  | 🟡 2/4/0 · P2 |
| **Clustering / HA / replication / failover / node drain** |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ ×2 | ✅ ×2 | ✅ |  | ✅ |  |  |  |  |  | ✅ 6/1/0 |
| **Secrets management (encrypted at rest, secret manager hierarchy, roles, SCIM)** |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | ✅ |  |  |  |  |  |  | ✅ |  | 🟡 0/3/0 · P1 |
| **Resource footprint / scale limits / capacity docs (CCU per node, DB sizing)** | ✅ |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 0/2/0 · P2 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Checks (`checks scan`, `checks locked-files`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Developer user manager |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Dev fixtures (`musubi dev` seeds) ; no test-user state copy |
| Linux dedicated-server builds guide (cross-compile, WSL, Docker) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Engine build guidance |
| Microservice x64-only images; Apple Silicon cross-compile; port 6565 health check |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — WASM is arch-independent |
| OpenAPI tooling (`oapi download/generate`, `project generate-client-oapi`, `project generate web-client`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — OpenAPI served; `musubi idl` codegen for configs only |
| Project lifecycle commands |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi dev`, `module publish/canary/promote/rollback/versions`; no process mgmt (ps/stop… |
| Stats service |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Telemetry collector mgmt (`beam telemetry collector start/ps/stop`, `telemetry logs/push/prune/config`) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — OTel exporter config only |
| Token tooling (`token inspect`, `token from-refresh`), `config secret` (realm secret) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Key management via admin API; no token inspect CLI |
| `beam deploy plan/release/status/get/list/registry` |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi deploy` + module versions; no status/list of past deployments |
| `beam generate-interface`, `project generate-env` (`--include-secret`, `.env` output), `generate-properties`,… |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi idl`/`schema`; no env scaffolding |
| `beam init` / `login` / `logout` / `me` / `config` (local overrides `--set --no-overrides`) / `.beamroot` |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `beam project run --with-group` / `BeamServiceGroup` |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Single binary |
| `beam temp` (clear temp), `docs` (open docs) |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| App lifecycle: Dev vs Live, live-lock, unlock |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted environments dev→stage→prod with content promotion |
| Calendar (LiveOps calendar) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — console UI; live events via Zanshin |
| Classroom / bulk member invites (5.8) |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Compatibility flags per app |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — versioned OpenAPI/IDL and migrations; no per-app behaviour-compat switch board |
| Documentation per method with 10 language samples + llms.txt |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — README route table + OpenAPI; no per-language samples |
| Forums & Bootcamp |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Max Server Nodes / cost guardrails |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Multi-app teams, Super users, team usage |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `[[projects]]` + operator roles (server/admin/support/liveops/ops) on keys; no team/membe… |
| Private NPM registry support (`.npmrc`), deploy troubleshooting (node_modules in git, `ecosystem.config.js`,… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Scale strategy per location: `vertical` (one instance) or `horizontal` (several instances behind LB); instanc… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Agent skills repo |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| CI/CD guidance |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Load-test / launch planning |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Local container testing guide (SSH tunnel) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Pluggable external orchestrator |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Block-list enforcement across EAS block list and platform (console) block lists in lobbies/sessions/P2P/RTC |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi blocks (/v1/friends/block) enforced in chat/DM/invites/matchmaker avoid list; no c… |
| Steam Deck: compatible-only (untested, unsupported); Chromebook supported by EAC |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Development iteration guides |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Game server wrapper |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — any `command` runs, but readiness/health calls are required |
| Remote access intro |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Terminate sessions guidance |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Clean exit destroys rooms |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Container-based dedicated game-server hosting |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Max process lifetime / draining |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — node drain (`POST /v1/admin/cluster/nodes/{id}/drain`, SIGTERM) exists for musubi nodes;… |
| Process boot + port-bind readiness contract |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Session/match-based target (not persistent worlds) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Stateful routing (room pinned to one server) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Zero-downtime deployments |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| `hathora build create` / `hathora deployment create -b` |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — builds via `POST …/fleets/{id}/builds` and `musubi fleet push`; no image build step |
| `hathora deploy` one-shot build+deployment with `--from-latest` |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi fleet push fleet.toml` pushes spec; image build/push left to CI |
| `hathora log` |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no log capture |
| Asset model: Context (grouping, class relationships) + Complexity (Loot Box, Drop Table behaviours) on every… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| End-of-life 2026-04-15: Missions, Collectables, (old) Segments, Asset Packages, Asset Variations, Rental Asse… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | — — Note: musubi has quests (≈missions), timed item expiry (≈rentals), storage KV with public… |
| Pagination: cursor (`cursor`,`per_page` ≤100, default 10), offset (`page`,`per_page`, total/last_page), simpl… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player identifiers: ULID (primary), 12-char Public UID for display/sharing, legacy numeric id, guest player_i… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — UUID account ids + display_name; no short shareable public UID |
| Stage & Live dual environments with Review Changes / merge / revert — REMOVED (replaced by single environment… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi keeps multi-project/environments + content versions/rollback (`/v1/admin/content/v… |
| Account info *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Build sharing *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no build hosting (`rg -i "build shar"` none) |
| Cadence *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — |
| Console Inspector *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — data via admin routes only; no UI |
| Debug cloud workers *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — fleet provider shells (ssh/k8s) not wrapped by CLI |
| Deployment runs *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Environments *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Friends *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| KV store (legacy) *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Lobbies & matchmaking *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Logs viewer *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — stdout/OTLP only |
| Metrics reference *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — metric names in README, no reference page |
| Monitoring proxy *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Parallel live channels *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player Accounts *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Raw worker logs *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Regions API *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Reports *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Room tags & KV *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Rooms auto-close *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Shard selection *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Spawn/joining queue *(Star Citizen)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Voice *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — voice delegated to Aurix (token mint `/v1/voice/token`); no in-engine proximity integrati… |
| Worker connection limits *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| World autoprovisioning *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Client Patch Versions per logic version, per-platform patch compatibility requirements editable on server, ho… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — single min version only |
| Deployment troubleshooting guide, `metaplay debug logs/shell/server-status/collect-cpu-profile/collect-heap-d… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi doctor`, `/v1/admin/dev/profile` CPU profiles (README dev tooling row); no remote… |
| Developer Players: dashboard "Mark as Developer" (`api.players.set_developer`), `[DevelopmentOnlyAction]` all… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no developer-player flag (`rg -i 'developer' README` → dev tooling only); roles are opera… |
| Entity schema versions + `[MigrationFromVersion]` migrations on PlayerModel/GuildModel; oldest supported sche… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Hotfix releases (patch releases per major, e.g. R37.x), release every ~3 months, "Migrating to a New Metaplay… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — |
| Logic Versions: `supportedLogicVersions` range baked at build, Active Logic Version Range editable at runtime… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi: `min_client_version` / client version gate in config + maintenance of API `v1` (r… |
| Metaplay Docs Custom GPT, Docs MCP server (`metaplay llm-docs` glob/read/ripgrep/search), Portal MCP (inspect… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no MCP/agent tooling; only `openapi.json` |
| Rolling out updates playbook: deploy server supporting both versions → raise active range → ship client → dro… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WASM module canary/promote/rollback for server logic; NO maintenance mode (`rg -i mainten… |
| Secrets: `metaplay secrets create/update/show/list/delete`, `user-` prefix, multi-entry, `kube-secret://`, `a… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — config via env/TOML + K8s secrets in Helm; no secret URL resolver or CLI |
| Server time skip (environment-wide, irreversible, dashboard Settings → Time Skip; dev/staging only) for testi… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no clock override (`rg -i 'time_skip\|clock_offset'` → none) |
| Simulating a poor network connection (client-side latency/packet loss simulation) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `musubi bench` measures latency but has no client-side loss/latency injection (`rg -i 'pa… |
| Dedicated, non-overprovisioned infra; data isolation; management layer only shared |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | — — self-hosted |
| GDD templates for designing systems; sample projects (Hiro Store, Mage Mayhem, Achievements, Teams, Challenge… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — musubi has docs/ plan + examples in repo, no sample games |
| Hiro Exporter: Google Sheets -> Hiro JSON definitions (Economy, Achievements, Energy, Inventory) with direct… |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ❌ · P1 — musubi content bundles are JSON via API/CLI; no spreadsheet exporter |
| Builders: connect GitHub/GitLab/Bitbucket (OAuth or PAT, self-hosted Git on request), compile Go (`main.go`,… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi: `musubi deploy` manifest + `module publish/canary/promote/rollback` from CI (READ… |
| Configuration & runtime variables editable in dashboard (full Nakama YAML keys), server key and runtime HTTP… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Deployment zones on GCP (several regions, US/EU/Asia) and AWS on request; zone fixed at creation; no cross-re… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Onboarding: email/password, Google OAuth, SAML; account sessions management; Demo Title |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | — |
| Regions: GCP zones (e.g. `us-east1`, `asia-northeast3`, EU) and AWS (`us-east-1`, `eu-west-2`) on request; Na… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | — — self-hosted anywhere (Terraform Hetzner/OVH examples) |
| Rolling node upgrade; schema migrations forward/back; Go plugin dependency pinning must match server exactly… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ✅ |
| Satori deployments (managed LiveOps: events, messaging, live events, A/B tests, segmentation, analytics, flag… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — Zanshin (open-source Satori analog) integration for flags/live events/analytics; no A/B e… |
| AppVersion isolation |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — musubi `appversion` appears in 3 files (matchmaking filters); no first-class pool isolati… |
| Auto-scaling & self-healing cloud |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — musubi fleet autoscaling/scale-to-zero (README fleets) but you operate the infra |
| Best Region selection |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Encrypted Photon token |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Managed regions |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — musubi georouting + fleets per region (README fleets/georouting) but you provision the re… |
| Migration notes PUN Classic→PUN 2, PUN vs Bolt |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Payload encryption |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Third-party pentests |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| 0.0.100 / 0.0.101 (2023) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 0.1.0–0.3.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 0.5.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 0.6.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 2025.2.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 2026.1.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 2026.2.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| 2026.3.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — historical |
| Deployment: managed AWS shards + Buildkite |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — musubi: Helm chart, Terraform (AWS, Hetzner, OVH), Docker, one binary. Self-serve vs vend… |
| Discord provisional accounts |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — device/guest accounts can be upgraded by linking (account merge), but no Discord Activiti… |
| Local single-node dev environment |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |
| AI skills package `npx skills add rivet-dev/skills`, GitHub Action to publish skills; docs "Copy Markdown" pe… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ❌ · P1 — no agent skill files; README is the only reference (`rg -il "skill"` only gameplay contex… |
| Hetzner / AWS / GCP / VM deploy guides for workers |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ |
| Rivet Cloud managed control plane + storage, global edge, 60-second deploy, Connect tab onboarding |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | — — hosted offering; musubi is self-host only (n/a per brief, but note: no managed option exi… |
| Testing: `setupTest` (Vitest) isolated registry, fake timers for schedules |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — `musubi-sim-sdk` simulated clients + integration test suites; no module unit-test harness… |
| AI-agent integration |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 |
| Database rename / aliases |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Deletion protection |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Incremental migration pattern |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Login (`spacetime login`) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — admin API keys |
| PostgreSQL wire protocol (PGWire) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — data already lives in Postgres (ahead: any PG tool/BI works, full SQL) |
| Project config `spacetime.json` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| Tutorials / sample games |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — README + docs; no sample game clients |
| Version management |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — cargo/Docker tag |
| `spacetime build` with lint |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `musubi module` subcommands; `musubi doctor` checks deployment, not module lints |
| `spacetime call` / `describe` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `GET /openapi.json`, `GET /v1/admin/idl` describe the API; RPC callable via curl, no CLI… |
| `spacetime dev` watch loop |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| `spacetime init` project templates |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — `musubi dev` + `musubi config`; no project/module scaffolding templates |
| `spacetime logs` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — server `tracing` logs to stdout; no CLI to tail remote module logs |
| `spacetime mcp` (Model Context Protocol server) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ❌ · P1 — grep `mcp` = 0 |
| `spacetime publish` |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |
| `spacetime sql` ad-hoc queries |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — Postgres: use `psql` directly (full SQL) |
| Builds / depots / branches |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — game-content distribution is out of scope; musubi does fleet server builds only |
| Compliance |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Config publishing workflow |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Engine integrations |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — HTTP/WS only today |
| Observability for partners |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Playtest program |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | 🟡 · P2 — login queue + LiveOps flags can gate access; no signup funnel |
| Rate limits per publisher key |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Release cadence |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | — — musubi alpha, pushes to main |
| SDK / platform coverage |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ❌ · P1 — no client SDKs yet; Unreal scaffold only (docs/parity.md "SDK") |
| Server-side scripting |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Webhooks / events out |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  | ✅ |
| Account cancellation data policy |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — |
| OpenAPI specs for every service (admin + client) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Player Statistics service (statistics v1/v2 OAS) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Presence service (rooms + Vivox tokens) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | — — Presence is in Friends/status streams; voice via Aurix integration |
| Project environments across all services (≤25), Environment selector, Environments API, build/runtime behavio… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — One cluster per environment (`node.environment`), multi-project per cluster; no env switc… |
| Samples / use-case repository (seasonal events, daily rewards, battle pass, coupons, etc. built on Cloud Code) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | 🟡 · P2 — Features are native; few end-to-end sample games (`examples/`) |
| Schema validation / migrations / TTL |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Service account auth for Web APIs (key id/secret → Basic or stateless token; 20 rps / 18k per 30 min) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Wire SDK (WebSocket push channel shared by Friends, Cloud Code push messages, Lobby) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Workflow Engine API |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  | ✅ |
| Boss Room / samples, casual co-op quickstart |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | 🟡 · P2 — README examples + `examples/` only; no engine sample game. |
| Dedicated Server package 2.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Multiplayer Center |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Multiplayer Play Mode 2.0 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| Multiplayer Tools 2.2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — — Server side: request recording/replay, fixtures, `musubi bench`, `musubi soak` (CLI). Cli… |
| Multiplayer Widgets |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |

### 2.19 console/admin (8 normalized features, 100 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **Web admin console (player management: search, profile edit, identities, bans, grants, data edit)** | ✅ ×2 |  | ✅ | ✅ ×2 |  |  | ✅ | ✅ | ✅ | ✅ ×2 |  | ✅ |  | ✅ |  | ✅ ×19 | ✅ ×7 |  | ✅ |  | ✅ |  | ❌ 5/10/24 · P0 |
| **Observability UI (logs, metrics, usage, API usage page, billing/usage views)** |  |  | ✅ ×2 | ✅ |  |  |  |  | ✅ |  | ✅ | ✅ |  | ✅ |  |  |  |  |  |  |  | ✅ | ❌ 1/3/3 · P0 |
| **Admin API (REST surface for everything the console does)** |  |  |  |  |  |  |  |  |  | ✅ ×9 |  | ✅ |  |  |  |  |  |  |  |  | ✅ ×2 |  | 🟡 5/5/0 · P1 |
| **Console access: roles/ACL templates, MFA, SSO, audit trail, team invites** |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ | ✅ |  |  |  |  |  |  |  | 🟡 1/2/0 · P1 |
| **API explorer / request console / data reset tools** |  |  |  | ✅ |  |  |  |  |  |  |  |  |  | ✅ ×2 |  |  |  |  |  |  |  |  | 🟡 1/2/0 · P1 |
| **Deployment / checkpoints / environment management UI (promote content, rollback, config export)** |  |  | ✅ | ✅ ×3 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ 3/0/1 |
| **Content/config editors (economy, leaderboards, achievements, campaigns, remote config, segments) with publish workflow** |  |  | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/1/1 · P0 |
| **Live inspectors (rooms/matches/lobbies/worlds, sessions, send-as-client, force disconnect)** |  |  |  |  | ✅ ×3 |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ 0/1/2 · P0 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Announcements / Groups / Leaderboards / Tournaments / IAP (Real-Money Transactions) pages |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No admin console (README: console not yet written) |
| Beamable Console (successor) + MicroViews extensions |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No console, no extension framework |
| Notify All Players |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player admin page |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — All admin APIs exist (`/v1/admin/accounts*`, inventory, storage); no web UI |
| Team user administration & roles |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Operator RBAC roles + OIDC SSO + per-project scoping (README admin rows); no realm-hierar… |
| `@colyseus/testing`: boot server from `app.config.ts`, `createRoom`, `connectTo`, `waitForMessage`, `waitForN… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Rust integration-test harness in repo (CI full suite vs Postgres) + `musubi dev` fixtures… |
| Analytics (PAYG) |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus/OTel export + usage/demand JSON; operator brings Grafana |
| Billing |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Notifications |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Audit log + webhooks; no operator notification center |
| Testing & Releases scheduler |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `ScalePolicy.schedule[{days,from_hour,to_hour,buffer}]` scales by schedule; no event plan… |
| Web dashboard |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — README: admin console not written; everything is API/CLI |
| Bulk Importer/Exporter for Stats + Achievements (zip of stats.csv, achievementDefinitions.csv, achievementLoc… |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Developer Authentication Tool (local credential server) |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — see Auth row |
| File Decryption Tool (CLI) for Title Storage / Player Data Storage encrypted files (hex key ≤64 chars) |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — musubi storage is not client-side encrypted |
| Allocation latency SLO |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Application settings grouped Application / Deployment / Advanced |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — TOML config |
| Auto-refresh, validation errors, UI fixes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Billing UI: invoices, upcoming invoice, payment method, Stripe portal |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted |
| Block / object storage for workloads |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — game-server scope; musubi storage is player data (`musubi-storage`) |
| Bullet Mania open-source sample |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Cloud burst providers |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator chooses; Terraform for Hetzner/OVH/K8s |
| Commitment-based bare-metal ordering |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Console/API e-mail verification gate |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Create room from Console with per-region ping |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — API only |
| Customer references |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — no public production references |
| Deployment page redesign, app overview with buildTag, copy processId |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no console |
| Drag-and-drop build upload |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no managed builds |
| Explore-tier hard limits |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Fleet tab: capacity graphs, cloud minimums, node table |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — API only |
| Invoice line items by region & hardware |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Logs viewer no longer freezes on high volume; aggregated app logs removed |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Minecraft "no code changes" demo deploy |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Multi-cloud / multi-provider scheduling |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `allocate_routed` by region/pool; no cost-aware node bin packing |
| Node scale-up latency |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — no node provisioning |
| Private networking mesh between nodes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg wireguard/vpn` → 0; Terraform private networks per provider only |
| Process table with filters, counts, copy id, logs, metrics, stop |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — API only |
| Region filter → 10 most recent started/stopped processes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — API filtering by fleet only |
| Regional failure isolation |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Search stopped room/process → logs + metrics in one click |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Security patching via draining |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — node drain exists; game-server hosts patched by operator |
| Student / non-profit credits, $500 starter credit |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Teams UI: invites, roles |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no console; roles via admin keys |
| API Keys page: API Domain + Domain Key per game, multiple Game API keys (per platform/version), Server API ke… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Collectables (legacy, EOL) and Equipment Slots / entity loadouts (`get-my-loadout`, `get-an-entity-loadout`,… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Feature toggles per game (Get Feature Toggle List / Toggle Feature); Preview builds (Start Preview Build, Get… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | — — Vendor-hosted page builds; musubi flags (`/v1/liveops/flags`) cover feature gating |
| Game Operations log (date UTC, event code e.g. `player.deleted`, description) of every console/API operation |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Operations log per player (balance_credit etc. with source) in console |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Organization users with roles Admin / Developer (no Game Settings) / Moderator (leaderboards, UGC, reports, p… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Admin keys with roles + operator SSO (`/v1/admin/sso`, `0094_operator_sso.sql`, parity.md… |
| Players Takeout (GDPR data export per player), Selective Player Profile Deletion (May 2026: choose which data… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Public/Private profile toggle (private disables Steam inventory integration / hides items) |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — No Steam Inventory Service integration (rg 'steam inventory' = 0) |
| Referral Programs: create/update/delete programs, player referral invite (`get-referral-invite`), list player… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — rg -i referral = 0 |
| Send individual notification from console (type, priority, JSON content, properties), read status |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `POST /v1/server/notifications` with code/content; no priority field (rg priority in noti… |
| Wallet Modifiers / Currency Multipliers (Jun 2025): per-player multipliers applied to currency grants; Bulk b… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Reward modifiers (`/v1/rewards/modifiers`, README reward modifiers: multipliers/time-boxe… |
| Assume Roles (test permissions when Auth off; per-tab header) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no role impersonation |
| Custom dashboard development: Vue 3 + TS project scaffolded by `metaplay init dashboard`, `gameSpecific.ts` r… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no dashboard |
| Custom dashboard pages/components via Integration API (`addUiComponent`, placements, `displayPermission`), Me… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no console |
| Dashboard SSO authentication (Google, GitHub, Atlassian; third-party IdP) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Dashboard data subscriptions (polling with args, dynamic/static/custom subscriptions) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — dashboard internals |
| Dashboard subscriptions: polling data sources with shared cache (`getSinglePlayerSubscriptionOptions` etc.),… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — |
| Database scan jobs UI: list/create/pause/cancel jobs (e.g. deletion sweeps, custom migrations), progress %, t… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Debug production player locally: export from cloud → import to local → Reconnect Devices to attach own login |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — export/import possible; no identity re-attach tool |
| Entity export/import: Entity Archive (base64 serialized player/guild + members) download/copy, import with ne… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Environment header color per environment |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — UI concept |
| Environment page: node list with workload/drain status, database shard info, cluster status, deployment info |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET /v1/admin/cluster/nodes` + health endpoints (README cluster row); no UI |
| Event stream viewer (player/guild event logs per entity) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — server event bus (`/v1/server/events`) streams to Zanshin/webhooks; no per-entity history… |
| Event streams: Latest Player Events + Latest Audit Log Events per player, filter/highlight, keyword presets |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — audit log per account via `/v1/admin/audit?account_id`; no player event log |
| Events/offers page: view config + live behaviour, per-player preview of how an event appears for that player'… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no native in-game event/offer system (LiveOps delegated to Zanshin) |
| Game config management page (view, build history, diff, publish, rollback) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — content bundle versions/diff/rollback exist via API (`/v1/admin/content/*`), no UI |
| Generated forms & views from C# types (schema-driven), server-side form validation, custom field decorators |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — storage JSON Schema exists but no UI generation |
| Generated forms/views from C# types (`[MetaFormDisplayProps]`, `[MetaValidate*]`, localized fields, logic-ver… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 |
| Guild pages: guild list/search, guild details, member list, event log, admin actions (edit, kick), export gui… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — admin group routes exist, no UI |
| Metaplay Portal: org/project/environment management, member roles synced to dashboard, deployment status, ser… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — SaaS console |
| Player details page with 5 customizable tabs, UiPlacement slots |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no UI; player data only via `GET /v1/admin/accounts/{id}` |
| Player search by ID, name (prefix/full-text), social platform ID (R33), deleted-player lookup |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Player search by name (case-insensitive), by ID, by social ID `<Platform>:<id>` (Facebook/Steam/SignInWithApp… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ✅ |
| Quick links to external resources |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — UI concept |
| Reconnect Devices tool: move login methods between accounts, per-method checkbox, preview, audit on both acco… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — musubi has link/unlink identities + account merge (`/v1/account/link/*`, `/v1/account/mer… |
| Safety lock on dangerous actions |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — UI concept; musubi has no console |
| Server error logs page (`api.system.view_error_logs` permission), recent server errors/exceptions with counts |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — logs only via stdout/OTel |
| Groups page (search by id/name, edit members/roles), Storage browser (collection/key/user filter, `key%` pref… |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | 🟡 · P2 — all backed by admin routes in musubi (`/v1/admin/storage`, `/v1/admin/chat/*`, leaderboar… |
| `RegisterConsoleHttp` custom console HTTP handlers (3.34) |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  | ❌ · P1 — modules expose RPC/hook surfaces only (rg -i console_http → none) |
| Anonymous clients toggle |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ✅ |
| Counter analytics |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — Prometheus metrics + OpenTelemetry (README observability) but no built-in charts UI |
| Dashboard config keys |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — server-only match creation enforced by server keys; tick config in match config; no dashb… |
| Plugin/webhook key-value config |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | 🟡 · P2 — musubi `config` service (migrations 0113 configs, content environments) but no per-app we… |
| Support tickets from dashboard |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Web dashboard: AppIds |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | ❌ · P1 — no admin console UI (README "not written yet"); admin via HTTP API/CLI only |
| Autoscaling: core services fully managed/elastic (no studio action); MPS standby scaling (default/scheduled/d… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Title limits page: configurable per-feature limits (economy, data sizes etc.) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | ✅ |
| Portal customization framework |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — — nothing to extend. |
| Portal extensions (custom portlets/pages calling custom RPCs) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — |
| Portal: Configuration viewer & Feature Toggles |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | 🟡 · P2 — `GET /v1/admin/config` (redacted) exists; no toggles UI. |
| Portal: Data Rights page |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 |
| Portal: Game Servers page |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 — `GET /v1/fleets/*` API only. |
| Portal: Game Titles / Shards / Access Lists |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 |
| Portal: Icons / Styles / Swappable components |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | — |
| Portal: Players page (Game Operator) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 — API only. |
| System logs viewer |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ❌ · P1 — logs go to stdout/OTLP; no admin log-tail route (`rg -i 'GetLogs\|/logs'` none). |
| Team collaboration / orgs |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | 🟡 · P2 — operator RBAC + SSO (`0094`) for API; no org/project hierarchy |
| User management (SpacetimeAuth) |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | ✅ |

### 2.20 compliance (8 normalized features, 47 single-competitor features)

| feature | AccelByte | Agones/OM | Beamable | brainCloud | Colyseus | Edgegap | EOS | GameLift | Hathora | LootLocker | Meshing vendors | Metaplay | Hiro/Satori | Nakama | Photon | PlayFab | Pragma | Rivet | SpacetimeDB | Steamworks | UGS LiveOps | UGS Multiplayer | **musubi** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **GDPR data export / erasure (recorded delete, tombstones, async export jobs, download links)** | ✅ ×2 |  |  | ✅ ×3 |  | ✅ ×2 | ✅ ×2 | ✅ | ✅ ×2 | ✅ ×2 | ✅ | ✅ ×3 | ✅ | ✅ |  | ✅ ×2 | ✅ ×2 | ✅ ×2 | ✅ ×2 | ✅ | ✅ | ✅ ×2 | ✅ 16/11/0 |
| **Licensing / open-source terms / revenue share / usage caps** |  |  | ✅ ×2 | ✅ | ✅ |  | ✅ |  |  | ✅ | ✅ ×2 | ✅ ×2 | ✅ | ✅ ×2 | ✅ | ✅ ×2 |  |  |  | ✅ |  |  | ✅ 5/1/0 |
| **Age gate / COPPA / parental consent / verifiable consent providers (KWS-style)** |  |  |  | ✅ |  |  | ✅ ×3 |  |  |  | ✅ |  |  | ✅ |  | ✅ | ✅ | ✅ |  |  |  | ✅ | ✅ 5/1/1 |
| **Security certifications / SOC 2 / ISO 27001 / pen tests / encryption at rest** |  |  |  |  |  |  |  | ✅ | ✅ |  | ✅ |  |  |  |  |  |  | ✅ |  |  |  | ✅ | — 0/0/0 |
| **Legal agreements / ToS / privacy policy versioning and acceptance tracking** | ✅ |  |  |  |  |  | ✅ ×3 |  |  | ✅ | ✅ ×2 |  |  |  |  |  |  |  |  |  |  |  | ❌ 2/0/4 · P0 |
| **Login allow-list / limited access / region-locked playtests (compliance side)** |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  | ✅ |  | ✅ |  |  |  |  |  | 🟡 0/2/1 · P1 |
| **Audit log of operator actions** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ 1/0/0 |
| **Data retention policies / configurable TTL per table** |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | ✅ 1/0/0 |
| *single-competitor features* |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | |
| Age restriction: global min age + per-country overrides for account creation | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — age gate brackets child/teen/adult with feature closures + parental consent (`[age_gate]`… |
| WebSocket close code contract: 4000 CONSENTED, 4001 SERVER_SHUTDOWN, 4002 WITH_ERROR, 4003 FAILED_TO_RECONNEC… |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — errors carry codes/`trace_id` in `error` envelopes; close reasons like `unavailable: slow… |
| Support channels |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — GitHub issues |
| Support: eoshelp.epicgames.com ticketing, FAQs, forums; status page |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — community/self-support |
| Cost optimization guidance |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — vCPU-hour usage per build/region (`GET /v1/server/fleets/{id}/usage`); no cost-aware plac… |
| Pricing model & calculator |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Apache-2.0, infra cost only |
| Resource tags |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — fleet labels (`POST /v1/gameserver/label`), no cost tagging |
| Bare-metal commitments |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Basic DDoS protection on every node |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — `rg ddos` → 0; UDP edge connect tokens drop unauthenticated packets but no volumetric mit… |
| Compute isolation / single tenancy |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Enterprise tier |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Explore tier (free) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — Apache-2.0, no usage caps |
| Legacy plan sizes |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Logs encrypted at rest, TLS everywhere |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Private container registry, signed-URL uploads over HTTPS |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator's registry |
| Pro tier |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Published rates |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — |
| Revocable tokens |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Scale claims |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `musubi bench`/soak numbers in README (1 M WS pings/s on one node) but no public producti… |
| Usage billing units |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — `GET …/fleets/{id}/usage` vCPU-hours per build for chargeback; no egress metering |
| Vulnerability disclosure contact |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — open-source repo issues |
| Zero-config VPN between servers and backend (Tailscale beta) |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  |  | — — operator network (Terraform modules create private networks + firewalls) |
| Error schema: request_id, trace_id, code, doc_url, message, param; documented error codes |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — Stable error codes in JSON responses; no request/trace id or doc_url in error bodies (rg… |
| Pre-launch checklist, WebGL build notes, Discord support (weekday CET), Unreal Marketplace plugin support |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | — — Vendor support process |
| SDK Error Reports: client reports failed requests (user_description, SDK version, trace_id, redacted request/… |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — rg -i 'error report' = 0; musubi has server-side request recording/replay (`musubi record… *(verdict changed, §5.3)* |
| Credit price table *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted |
| Enterprise gating *(coherence)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — musubi has no CCU caps (README benchmarks 2 000 CCU/node) |
| Infrastructure types *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | ✅ |
| Pricing model *(Improbable SpatialOS)* |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  |  | — — self-hosted |
| Backup re-deletion safeguard (players deleted then restored from backup are re-deleted) |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no backup-restore re-deletion ledger |
| Custom audit events from game-specific admin endpoints |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | 🟡 · P2 — WASM modules cannot append custom audit entries; audit is fixed to built-in admin routes |
| Environment tiers: dev envs use in-cluster DB; prod uses Aurora MySQL; observability retention mini 3h / smal… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — |
| Plans: Free (local dev), Starter €195/mo (≤5 DAU, 1 mini env), Pre-launch €995/mo (≤100 DAU, 2 small envs), P… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | — — Apache-2.0, pay only infra |
| Player IP geolocation: MaxMind GeoLite2 Country/City auto-download + weekly updates, auto-disable on stale DB… |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  |  |  |  | ❌ · P1 — no GeoIP (`rg -i 'geoip\|maxmind'` → none); only region hints for fleet routing |
| CCU accounting |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — — musubi exposes CCU via Prometheus but has no billing concept |
| Free100 commercial tier |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Premium Cloud |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Public Cloud free dev tier |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — — self-hosted; no CCU gating anywhere in musubi |
| Public Cloud paid tiers |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  |  | — |
| Plans: Free to Start (dev mode only, 100k users/title, 10 titles/studio, dev-mode quotas listed), Pay-as-you-… |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  |  |  | — — Apache-2.0, infra cost only |
| FoundationDB storage driver, multi-tenancy layer |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — Postgres + Raft cluster; multi-region clustering free (README L232), but no FDB-scale bac… |
| SQLite backup + PITR, actor forking |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  |  | 🟡 · P2 — match snapshots/journal/replay (`GET …/replay`), per-account backup/restore; no PITR or f… |
| Energy metering & spending limits |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — — (musubi has per-module fuel caps and economy quotas, not billing) |
| Enterprise per-node pricing |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Free tier limits |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ |  |  |  | — |
| Apple privacy manifests / Google data safety per SDK |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |
| DSA notices |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  |  | ✅ | — |

## 3. Gap backlog for musubi

Every `missing` and `partial` inventory row is here exactly once: 174 deduplicated backlog items (§3.1–3.3, covering 1056 merged rows) plus 681 single-competitor items (§3.4). Priority: **P0** = several major competitors have it and studios ask for it; **P1** = one or two competitors (or many competitors but musubi is already partial); **P2** = nice-to-have. Within a priority, items are sorted by how many competitors have the feature. "Competitors with it" counts reports that list the feature at all (including rows where musubi already has part of it); the `h/p/m` triple is musubi's row verdicts.

### 3.1 P0 — 63 items

#### P0-1. Unity / C# client SDK (typed REST + realtime client, session refresh, offline cache, UPM package)  
*area:* SDKs · *musubi:* ❌ missing (6h/18p/45m over 80 rows)
- **Competitors with it (20):** AccelByte, Agones/OM, Beamable, brainCloud, Colyseus, Edgegap, EOS, GameLift, Hathora, LootLocker, Metaplay, Hiro/Satori, Nakama, Photon, PlayFab, Pragma, Rivet, SpacetimeDB, UGS LiveOps, UGS Multiplayer
- **Why it matters:** Unity is the largest engine by studio count; no SDK = no adoption.
- **Implementation sketch (musubi terms):** Missing. Generate C# REST client from `openapi.json` + hand-written realtime client (WS/QUIC) with IDL codegen → `sdk/unity` UPM package; auto session refresh, offline cache.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Game SDKs: Unreal SDK + OSS + NetworkUtilities (UE 5.5–5.8; 5.0–5.4 deprecated 2026.3), Unity SDK (+ Unity P2P WebRTC p…* — no client SDKs in repo snapshot (README: HTTP/WS/QUIC + `GET /openapi.json`); Unreal first planned
  - ❌ AccelByte: *Sample game "Byte Wars" (Unreal + Unity) tutorial modules covering every service incl. AMS; ~524 tutorial pages* — examples/ dir with scripts only; no reference game
  - ❌ Beamable: *Content Manager (Unity) w/ CLI backend* — No editor tooling for content authoring
  - ❌ Beamable: *Unity SDK (`com.beamable`, `com.beamable.server`)* — No Unity SDK (README: SDKs not yet written)
  - ❌ Beamable: *Unity `BeamContext` lifecycle, DI (`IDependencyProvider`), observables, Promises, `RequesterException` shared with micr…* — No SDK
  - ❌ Beamable: *Unity SDK request headers (SDK/engine/game version)* — No client version header convention
  - ❌ brainCloud: *Client libraries* — only Unreal scaffold (`sdk/unreal`); README: client SDKs not written
  - ❌ brainCloud: *Example projects & Bootcamp videos* — bench/soak tools only
  - ❌ Colyseus: *Custom callback system per SDK (reference implementations JS/Unity/Defold/Haxe); `@colyseus/react` hooks `useRoom`, `us…* — no React/JS SDK
  - ❌ Colyseus: *Client prediction `Predict.get(room)`: `reconciler` (shared deterministic `step(ctx,state,cmd)`, `smoothMs`, `snap`, `w…* — server has deterministic rollback/journal + snapshots (`0064_match_recordings`, parity "deterministic rollback"), but no client prediction/interpolation library (no SDK) — `rg -i 'reconcil\|interpolat'` only in server/w…
  - ❌ Colyseus: *Official SDK matrix: JavaScript/TypeScript (`@colyseus/sdk`, React hooks `@colyseus/react`), Unity (UPM, C#), MonoGame…* — README: "client SDKs not written yet"; integration = HTTP + WS/QUIC/WebTransport/WebRTC/UDP by hand, IDL `musubi idl gen` emits typed state structs (Rust/TS/C#) only
  - ❌ Edgegap: *Unity Dedicated Servers quickstart plugin* — No Unity integration
  - ❌ EOS: *Unity: EOS Plugin for Unity (github EOS-Contrib/eos_plugin_for_unity) + manual C# integration; Android/iOS/Windows guid…* — no Unity SDK (rg -i unity → none)
  - ❌ GameLift: *Client SDKs (player gateway + ping beacons)* — no client SDK (only Unreal scaffold); clients speak raw WS/UDP protocol from `musubi-proto`
  - ❌ GameLift: *Unity plugin* — `rg -i unity` → README mention only
  - ❌ Hathora: *Sample games & Dockerfiles per engine* — no engine samples
  - ❌ Hathora: *Dockerfile authoring guide + sample Dockerfiles (Unity, Unreal, Godot, Node, Minecraft demo)* — no game-server Dockerfile samples (only musubi's own Dockerfile)
  - ❌ LootLocker: *Unity SDK (2019.1+, OpenUPM / git / unitypackage, editor extension with console login + API-key picker, debug level, au…* — No Unity SDK (rg -i unity in sdk/ = 0)
  - ❌ Metaplay: *Unity Model Inspector (browse live PlayerModel/GuildModel in editor), Metaplay Status window, offers debug tool, overri…* — no engine tooling
  - ❌ Metaplay: *Supported platforms: Unity (iOS, Android, Windows, macOS, WebGL), Unity 6.2 support; EDM4U Android dependency plugin* — Unreal scaffold only; no Unity/Unreal-mature/Godot/web SDKs
  - ❌ Hiro/Satori: *Hiro client SDKs: Unity/.NET (DLL, UnityPurchasingSystem, Mobile Notifications, Facebook/Google/Apple systems, analytic…* — musubi: no client SDKs yet (README status), OpenAPI 3.1 `GET /openapi.json` + IDL for generation; typed schemas "come with the SDK generation work"
  - ❌ Hiro/Satori: *Offline Support (Unity): local state + sync for Achievements, Economy, Energy (incl. modifiers), Inventory, Stats, Stre…* — none
  - ❌ Nakama: *SDK auto-refresh of session (checks expiry within 5 min) in .NET/Unity, JS, Java, Godot, Swift; not Dart/Unreal/C++/Def…* — musubi has no client SDKs besides Unreal (`sdk/unreal`); README: "client SDKs ... not written yet"
  - ❌ Nakama: *Official open-source client SDKs: Unity/.NET, Unreal (C++), C++, JavaScript/TypeScript (incl. Cocos2d-x JS), Java/Andro…* — only `sdk/unreal` exists in musubi; README: "client SDKs … not written yet" — biggest adoption gap
  - ❌ Nakama: *Console SDKs for Xbox Series X/S, Xbox One, PS5, PS4, Nintendo Switch for C++/Unreal and Unity — closed source, provide…* — no console client SDKs (server-side Xbox/PSN/Nintendo auth exists)
  - ❌ Nakama: *Tutorials/sample games: Pirate Panic (Unity: auth, clans, friends, leaderboards, matchmaking, multiplayer, notification…* — no sample games/tutorials in musubi repo (only README + docs/)
  - ❌ Photon: *Chat client SDKs* — no client SDK (HTTP/WS only)
  - ❌ Photon: *Client SDK coverage* — only Unreal SDK scaffold (`sdk/unreal`); no Unity/C#/JS/C++/Godot SDKs (README: "client SDKs not written yet")
  - ❌ PlayFab: *Client/Server SDKs (generated via SdkGenerator): C# (.NET, Unity, Xamarin), Unity3D (asset package + Editor Extensions)…* — README: no official client SDKs yet (alpha); OpenAPI + `musubi idl` generate typed stubs (TS/C#/C++/Rust/GDScript/UE) — generated code, no maintained SDK packages
  - ❌ PlayFab: *Unity SDK: Editor Extensions (title id/secret config, SDK install/upgrade), PlayFabSettings (TitleId, RequestType WWW/U…* — IDL-generated C# only
  - ❌ PlayFab: *C# SDK (.NET Standard; instance APIs PlayFabAuthenticationInstanceApi etc. for multi-player-per-process; CompressRespon…* — IDL stubs (TS/C#/C++/Rust/GDScript/UE) require your own HTTP/WS glue
  - ❌ Pragma: *Unity SDK* — README: "client SDKs … are not written yet" apart from Unreal.
  - ❌ Pragma: *Unity SDK project setup* — no Unity SDK.
  - ❌ SpacetimeDB: *C# client SDK (Unity / .NET / Godot)* — none (parity.md: "SDK клиентов 🟡 Unreal-скаффолд, остальные нет")
  - ❌ UGS LiveOps: *Unity Editor integration (Deployment window, service settings, module references)* — No engine editor tooling (Unreal scaffold only)
  - ❌ UGS Multiplayer: *Unity C# SDK for backend services* — No Unity SDK (README: client SDKs not written; `sdk/unreal` alpha only). Largest adoption gap for Unity studios.
  - 🟡 AccelByte: *Server/Extend SDKs: Go, C#, Java, Python (generated from OpenAPI specs in accelbyte-go-sdk/spec), modular SDKs, iam-go-…* — OpenAPI spec + typed IDL for modules; no published language SDK packages
  - 🟡 Agones/OM: *Official game-server SDKs: Unreal (plugin, full incl. counters/lists, auto `Connect`/`Health` in `BeginPlay`), Unity (n…* — `sdk/unreal` exists (client-side plugin, parity.md "client SDK and GDK intentionally after"); every other language must call the HTTP SDK (`GET /v1/gameserver`, `POST /v1/gameserver/*`) by hand — no published libraries
  - 🟡 Agones/OM: *Example servers: simple-game-server (Go, multi-arch, CRASH/EXIT commands), cpp-simple, nodejs-simple, rust-simple, unit…* — README quick start + `musubi dev`; no sample dedicated-server integrations per engine
  - 🟡 Beamable: *Autogenerated C# SDK from OpenAPI (`beamable-oapi.json`)* — OpenAPI spec published; users must run their own generator
  - 🟡 brainCloud: *Unreal Online Subsystem (BCOnlineSubsystem) & Unity Plugin (portal-connected, Builder API)* — `sdk/unreal` scaffold only
  - 🟡 Edgegap: *Sample projects* — `examples/` + bench scenarios; no engine sample games
  - 🟡 Edgegap: *Unity Gaming Services allocator* — `migrate-playfab` imports UGS data; no UGS matchmaker bridge
  - 🟡 GameLift: *Server SDK 5.x languages* — musubi game-server contract is plain HTTP (`/v1/gameserver/*` with server key + `X-Musubi-Server-Id`, env `MUSUBI_*`); no language libraries except Unreal scaffold (`sdk/unreal`)
  - 🟡 GameLift: *Engine integration guide (Unreal/Unity/custom)* — README engine notes
  - 🟡 Metaplay: *BotClient: headless C# client running N concurrent bots with game-specific AI (`BotClient.OnUpdate`), local or against…* — `musubi bench --scenario http\|storage\|ws\|chat\|match\|world\|all` + `musubi soak` (README CLI row) are protocol-level generators, not scriptable game-AI bots; no deployable bot fleet
  - 🟡 Metaplay: *Server-validated realtime gameplay pattern (client simulates, server re-validates outcome) sample; Blazor WebAssembly b…* — authoritative matches + replay validation (README match row); no browser client
  - 🟡 Hiro/Satori: *Client social login systems Facebook/Google/Apple wired into Unity SDK; Unity local notifications; Unity Purchasing* — musubi server supports Google/Apple/Steam/OIDC/... auth tokens; no Facebook login (rg -i facebook -> none) and no client-side integration layer
  - 🟡 Pragma: *SDK generation from protobuf (custom APIs included)* — `musubi idl` generates types/RPC stubs (Rust/TS/C++) from IDL; OpenAPI spec available for generators; not integrated into a full Unity/Unreal SDK.
  - 🟡 Rivet: *Unity / Unreal / Godot / HTML5 engine plugins (v1, 2024, discontinued)* — Unreal plugin v0 scaffold (`sdk/unreal`, not CI-compiled; no binary framing/QUIC/reconnect), no Unity/Godot — but Rivet currently ships none at all
  - 🟡 SpacetimeDB: *Code generation of typed bindings* — `musubi idl` generates from IDL for realtime state; no client code generation from OpenAPI/IDL for SDK languages
  - 🟡 SpacetimeDB: *Mobile / console client support* — musubi has console auth/platform routes but no SDKs at all
  - 🟡 UGS LiveOps: *Bindings generation (module spec → C# client stubs)* — IDL → Rust bindings + OpenAPI; no C#/Unreal client stub generator
  - 🟡 UGS LiveOps: *Rate-limit response headers (`RateLimit-Policy`, `RateLimit`, `Unity-RateLimit`) and API lifecycle policy* — 429 responses from `[limits]`; `rg Retry-After` → 0, no RateLimit headers — sketch: emit `RateLimit`/`Retry-After` from the token bucket middleware
- **Already covered rows (6):** EOS: Clients + Client Policies in Developer Portal: trusted server / untru…; LootLocker: CORS allowlist for browser builds (default allow-all; restrict origin…; Photon: Discord Activities support; PlayFab: Display names: UpdateUserTitleDisplayName (3–25 chars, unique per tit…; UGS Multiplayer: Onboarding / migration from Unity; UGS Multiplayer: Service accounts & project roles

#### P0-2. Unreal client SDK / Online Subsystem plugin (finished, CI-built)  
*area:* SDKs · *musubi:* 🟡 partial (1h/22p/3m over 29 rows)
- **Competitors with it (15):** Agones/OM, Beamable, brainCloud, Edgegap, EOS, GameLift, Hathora, LootLocker, Meshing vendors, Photon, PlayFab, Pragma, SpacetimeDB, UGS LiveOps, UGS Multiplayer
- **Why it matters:** Unreal is the AAA default; musubi has only a scaffold.
- **Implementation sketch (musubi terms):** Finish `sdk/unreal`: REST + realtime, OSS interface implementation, CI build matrix (UE 5.4–5.8), sample project.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Unreal replacement types & semantic types for microservice codegen* — No Unreal codegen
  - 🟡 Agones/OM: *Unreal plugin: UE4/UE5, `AgonesComponent` with `HttpRetryPolicy`, Blueprint-callable, uses REST (HTTP) not gRPC* — `sdk/unreal` is a client plugin for musubi's player API, not a dedicated-server SDK
  - 🟡 Beamable: *Unreal SDK 2.3 (UE 5.6, iOS support new)* — `sdk/unreal` scaffold only (README) — far thinner
  - 🟡 brainCloud: *Room Server Manager / S2S libs for servers* — server keys + gameserver SDK HTTP routes (`/v1/gameserver/*`), `musubi-sim-sdk`; no multi-language S2S client libraries
  - 🟡 Edgegap: *Unreal quickstart plugin + Docker Desktop extension + EGIK (Betide, Fab)* — `sdk/unreal/Musubi` uplugin scaffold (client API), no build/containerize/deploy tooling
  - 🟡 Edgegap: *EOS lobby integration (Unreal)* — Lobbies native in musubi; no EOS bridge
  - 🟡 EOS: *Unreal Engine: Online Subsystem EOS + EOS Plus plugins (UE 4.27+), Online Services plugins (UE 5.1+, split Game Service…* — sdk/unreal scaffold (MusubiClient/Socket/Subsystem) only; no OSS integration
  - 🟡 GameLift: *Unreal Engine plugin* — Unreal SDK scaffold only; no editor deployment workflows
  - 🟡 GameLift: *Unreal plugin profiles / Anywhere / EC2 / container workflows / integrate* — Unreal scaffold only
  - 🟡 GameLift: *Server SDK Unreal reference* — Unreal scaffold
  - 🟡 Hathora: *Unreal onboarding* — Unreal SDK scaffold exists (parity.md); no deploy tooling
  - 🟡 LootLocker: *Unreal SDK (4.27+, Fab/Marketplace or GitHub, C++ + Blueprint, project settings config) + separate Unreal Server SDK* — `sdk/unreal` scaffold only (README: SDKs not written)
  - 🟡 LootLocker: *Unreal Server SDK: start server session with Server API key, create players server-side, impersonate players (act on be…* — Server-key routes operate on any account_id (`/v1/server/*`, custom auth `POST /v1/auth/custom` mints player tokens); no Unreal server plugin
  - 🟡 Meshing vendors: *Engine plugins* — Unreal scaffold only
  - 🟡 PlayFab: *Unreal Engine: PlayFab Marketplace plugin (core APIs), Party OSS plugin, GSDK Unreal plugin for MPS, Lobby/Matchmaking…* — README Unreal: GSDK-compatible server plugin/`musubi-sim-sdk` bindings, Unreal-first design; no client plugin package
  - 🟡 PlayFab: *Unreal SDK: Marketplace plugin (Blueprint + C++ APIs for all Client/Server/Admin/Entity calls), UE 4.x–5.x support (5.2…* — README Unreal-first: GSDK-compatible server plugin + generated UE stubs; no Blueprint client plugin
  - 🟡 Pragma: *Unreal SDK* — `sdk/unreal/Musubi` plugin (REST + WS subsystem) — not code-generated from the route table, less coverage of 600+ routes, no Blueprint-complete surface.
  - 🟡 Pragma: *Unreal reference examples* — examples/ in repo + tests; no listen-server/Steam P2P sample.
  - 🟡 SpacetimeDB: *Automatic reconnect in SDK* — server supports resumable sessions and refresh tokens; no client SDK implements reconnect/replay (Unreal scaffold only)
  - 🟡 SpacetimeDB: *Unreal Engine SDK* — `sdk/unreal` scaffold only (README: SDKs not written)
  - 🟡 UGS LiveOps: *Unreal Engine SDK for authentication* — `sdk/unreal` scaffold only; README says client SDKs are not written yet
  - 🟡 UGS LiveOps: *Unreal / native support for the LiveOps stack* — Engine-agnostic HTTP/WS/QUIC; Unreal SDK scaffold `sdk/unreal` — neither side is complete
  - 🟡 UGS Multiplayer: *Unreal SDKs* — `sdk/unreal` alpha.
- **Already covered rows (1):** Pragma: Unreal SDK setup

#### P0-3. Backend metrics (Prometheus/OTel) + shipped Grafana dashboards + alert rules  
*area:* analytics/observability · *musubi:* 🟡 partial (15h/9p/8m over 34 rows)
- **Competitors with it (13):** Agones/OM, Beamable, GameLift, Hathora, Meshing vendors, Nakama, Photon, PlayFab, Pragma, Rivet, SpacetimeDB, UGS LiveOps, UGS Multiplayer
- **Why it matters:** Operations baseline.
- **Implementation sketch (musubi terms):** Exists: Prometheus + OTel. Gap: shipped Grafana dashboards + alert rules under `deploy/grafana`, ServiceMonitor in Helm.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *`agones_gameserver_creation_duration`, `agones_gameserver_state_duration` histograms* — `ready_at`/`terminated_at` columns exist (mig 0067) but no histogram is exported
  - ❌ Agones/OM: *Grafana dashboards shipped in `build/grafana` (autoscalers, allocations, controller, gameservers, go processes, apiserv…* — No dashboards in repo (`rg -i grafana` → 0)
  - ❌ Agones/OM: *Grafana dashboards: gRPC (client/server request rate, error rate, bytes per call), Redis (uptime, memory, keys, CPU), G…* — No dashboards
  - ❌ GameLift: *Server telemetry architecture / setup / types* — no server-metrics pipeline
  - ❌ Rivet: *SQLite profiling (fingerprints, p95 per statement/transaction, slow-query logs)* — no per-query profiling; `GET /v1/admin/debug/profile` is a CPU profile only
  - ❌ Rivet: *Alertmanager / Prometheus rules / Grafana dashboards* — `rg -i grafana` = 0; no shipped dashboards or alert rules
  - 🟡 Agones/OM: *OpenCensus metrics → Prometheus (`agones.metrics.prometheusEnabled`, port 8080 `/metrics`), Prometheus Operator `Servic…* — Prometheus `/metrics` only (metrics.rs); no ServiceMonitor template, no Stackdriver exporter
  - 🟡 Agones/OM: *`agones_fleets_replicas_count{type=total/desired/ready/reserved/allocated}`* — Only observed state, no desired-vs-actual gauge
  - 🟡 Agones/OM: *OpenCensus → Prometheus (`/metrics`, scrape annotations), Jaeger tracing (`samplerFraction`, agent/collector endpoints)…* — Prometheus + OTLP traces + on-demand CPU profile (metrics.rs, telemetry.rs, README:admin); no bundled Grafana/Jaeger stack, no Stackdriver
  - 🟡 GameLift: *Server telemetry (OpenTelemetry in Server SDK)* — musubi exposes its own realtime tick/snapshot metrics for authoritative matches; external dedicated servers have no telemetry ingestion endpoint (`rg -i tick_rate crates/musubi-server/src/metrics.rs` → none)
  - 🟡 Pragma: *Metrics vendors: Datadog / StatsD* — Prometheus scrape + OTLP traces; no native Datadog/StatsD push (use Prometheus→DD agent).
  - 🟡 Pragma: *Managed Grafana + auto dashboards* — Helm chart ships Prometheus scrape config; no bundled Grafana dashboards (`rg -i grafana` → 0).
  - 🟡 Rivet: *OpenTelemetry traces: actions, HTTP handlers, actor-to-actor (routing/wake/retry), SQLite ops (no SQL text), queue link…* — OTLP export of server spans (`telemetry.rs`, `rg -i opentelemetry` 6 files); no cross-actor trace propagation into module calls or client→actor spans
  - 🟡 UGS LiveOps: *Observability API (OTel logs query, filter language, 10 rps / 2000 per hour)* — Prometheus `/metrics`, OTel traces, structured logs, `trace_id` in errors, CPU profile endpoint; bring your own log store
  - 🟡 UGS Multiplayer: *Observability API (OTel logs)* — OTel tracing export + Prometheus + audit log + CPU profile endpoint (README "Observability"); no stored-log query API (bring your own stack).
- **Already covered rows (15):** Agones/OM: Metrics `agones_gameservers_count` / `agones_gameservers_total` per f…; Agones/OM: Metrics: `backend_tickets_assigned`, tickets created/deleted, time-to…; Agones/OM: OpenTelemetry via sidecar (`OM_OTEL_SIDECAR`), Google Cloud Monitorin…; Beamable: OTel standard log attributes incl. root/parent trace ids & connection…; Hathora: Metrics export (Prometheus remote-write); Meshing vendors: Prometheus metrics; Nakama: Prometheus exporter (`metrics.prometheus_port`, `reporting_freq_sec`,…; Nakama: Metric exporting add-on: single authenticated Prometheus endpoint agg…; Photon: Photon Counters; Photon: Counter publishing protocols; PlayFab: Server ops APIs: RequestMultiplayerServer (BuildId or BuildAliasParam…; Pragma: Accessing data / ETL …

#### P0-4. Web admin console (player management: search, profile edit, identities, bans, grants, data edit)  
*area:* console/admin · *musubi:* ❌ missing (5h/10p/24m over 40 rows)
- **Competitors with it (13):** AccelByte, Beamable, brainCloud, EOS, GameLift, Hathora, LootLocker, Metaplay, Nakama, PlayFab, Pragma, SpacetimeDB, UGS LiveOps
- **Why it matters:** Every competitor has one; README says not written. #1 blocker for non-engineers.
- **Implementation sketch (musubi terms):** Build `console/` (SPA over `/v1/admin/*` + server-key/SSO auth): players, bans, inventory/wallet, storage editor, leaderboards, matches/rooms, fleets, modules, webhooks, audit. Phase 1: players + economy + moderation.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Admin Portal: namespace switcher, pinned sidebar, Lookup Users (profile, bans, entitlements, wallet, stats tab, player…* — no admin console (README: "console not written"); all admin via REST/CLI
  - ❌ Beamable: *In-game Admin Console (runtime overlay)* — No client-side debug console
  - ❌ brainCloud: *Design Portal (Portal-X)* — admin console “not written yet” (README status); all operations exist as `/v1/admin/*` routes
  - ❌ EOS: *Developer Portal Analytics: Game Analytics (CCU, retention, sessions by platform), Voice metrics, Anti-Cheat metrics, 3…* — no dashboards/console; Prometheus+Grafana left to operator
  - ❌ GameLift: *AWS console: fleets, builds, aliases, queues, matchmaking, dashboard, compute tables, log locations, clone fleet, direc…* — admin console UI not written (README); admin REST API + CLI exist
  - ❌ Hathora: *Web Console (console.hathora.dev)* — README: "admin console not written yet"
  - ❌ LootLocker: *Web Console: browser UI for everything, multi-game, multi-organization switching, home status summary* — README status: admin console not written yet
  - ❌ Nakama: *Embedded admin web UI on :7351 (Vue.js + Heroic UI since 3.30): Dashboard (per-node latency/RPC rate/in/out bandwidth,…* — musubi: no admin console UI (README status; parity.md "admin console UI not written"); admin REST API (`/v1/admin/*`, 93 routes) + Prometheus only
  - ❌ PlayFab: *Game Manager: web console — title overview dashboard (4h/24h/3d/7d; DAU, logins, new users, API calls, revenue), Player…* — No admin UI at all (README: admin console not written; only admin HTTP API + CLI)
  - ❌ PlayFab: *Studio & title management: studios (10 dev titles per studio in free mode), titles, team invites with roles, Microsoft/…* — musubi projects/environments (`projects`, env configs) exist as API/CLI (migration 0043 projects) but no console, no studio/team invite UI
  - ❌ PlayFab: *Players page: search by ID/name/email/linked account, player overview (last login, platform, location, created, lifetim…* — Account admin API (`/v1/admin/accounts/*`: lookup, ban, grant, export, erase) exists; no UI
  - ❌ PlayFab: *Economy UI: catalogs/items/bundles/containers/drop tables/stores/currency editors, coupon generation, Economy v2 catalo…* — All equivalents are JSON over admin API / CLI (`musubi schema`, `musubi module`, `musubi fleet`, store/items admin routes); no visual editors
  - ✅ PlayFab: *Title news: AddNews (Admin, Title/Body/Timestamp), AddLocalizedNews, GetTitleNews (count), ≤10 items (tier), news bell…* — `POST/GET/PUT/DELETE /v1/server/news[/{id}]` (title/body/published_at/expires_at/audience/tags/pinned/link/image/metadata/enabled), `PUT/DELETE …/{id}/locales/{locale}`, `GET …/{id}/stats`; client `GET /v1/news?count&locale&since&unread_only&tag` (pinned first, `de-AT`→`de`→base fallback, `unread` badge count, no tier cap), `POST /v1/news/{id}/read`, `POST /v1/news/read_all`
  - ❌ PlayFab: *Title overview dashboard: KPIs (DAU/new users/logins/API calls/revenue/errors) with 4h–7d ranges, API error list (top e…* — Prometheus metrics only (Grafana dashboards not shipped)
  - ❌ PlayFab: *Multiplayer UI: builds (create wizard, regions, standby/max, alias), servers list/details/logs, quota summary & change…* — CLI/API only
  - ❌ PlayFab: *Automation UI: Rules editor (trigger/conditions/actions), Scheduled Tasks (history, run now), CloudScript revisions (up…* — CLI `musubi module *` only
  - ❌ PlayFab: *Data UI: PlayStream Monitor, Event History, Data Explorer (basic/advanced), Trends, Reports (download CSV), Data Connec…* — None
  - ❌ PlayFab: *Engagement UI: Segments editor (filters/actions), Players page, Push/Email setup & templates, Title News, Content (file…* — None
  - ❌ PlayFab: *Economy/Progression/UGC UI: Economy v1 & v2 editors, Stores, Currencies, Coupons, Drop tables, Leaderboards & Statistic…* — None
  - ❌ Pragma: *Multi-game / shared social shard* — No title/shard registry; `rg -i 'title_id\|game_id\|shard'` only hits mesh shards (world cells).
  - ❌ Pragma: *Content Catalogs Editor (portal)* — admin API only; README: admin console not written yet.
  - ❌ Pragma: *Game Operator Portal* — no admin UI (README line 8). Zanshin LiveOps integration (`musubi-integrations`) covers LiveOps dashboards only.
  - ❌ Pragma: *Social Operator Portal* — admin API (93 routes) exists; no UI.
  - ❌ SpacetimeDB: *Web admin console* — admin console not written (README); admin REST API exists (`/v1/admin/*`, console-role keys)
  - 🟡 AccelByte: *Game records (namespace-level JSON), player records (private/public via is_public), admin records (game/player, server+…* — collections with Nakama permission codes (read 0/1/2, write 0/1), CAS, TTL, patch ops, schemas, indexes, full-text, subscriptions (README "Storage"); no first-class binary/blob records with presigned S3 upload for playe…
  - 🟡 brainCloud: *User Browser / CSR tools* — support-role admin routes exist (`PATCH /v1/admin/accounts/{id}`, ledger, inventory grants, bans, export) — no UI
  - 🟡 LootLocker: *Player search by platform id / public UID; player page: name, avatar, Public UID, first/last seen, currencies, platform…* — All operations exist as server/admin routes (`/v1/admin/accounts/*`, GDPR export/delete, storage, leaderboards `DELETE …/records/{owner}`); no web console, no public UID concept
  - 🟡 PlayFab: *Game Manager API feature settings: require valid JSON for custom data, disable all API access (maintenance), player IP…* — musubi: maintenance via login queue/config; JSON always required; no IP obfuscation toggle (grep obfusc: none)
  - 🟡 PlayFab: *Player profile: GetPlayerProfile with ProfileConstraints (ShowAvatarUrl, ShowBannedUntil, ShowCampaignAttributions, Sho…* — README public profile fields (display name, avatar, status, labels) + `/v1/users/{id}`; no per-field client visibility matrix, no LTV/location/origination fields
  - 🟡 PlayFab: *Add-ons marketplace page: Community Sift (profanity), SendGrid (email), Photon, Steam, Twitch, Apple, Google, Facebook,…* — Integrations configured via musubi.toml (`musubi-integrations`: email, push, IAP providers, Zanshin, Aurix, moderation/translation webhooks); fewer vendors (no Community Sift, Xsolla, PayPal)
  - 🟡 Pragma: *Roles & permissions (operator RBAC)* — 5 fixed key roles (server/admin/support/liveops/ops) with route-group scoping + audit log; no custom role composition or per-permission granularity.
  - 🟡 Pragma: *Player Groups (operator-defined cohorts)* — musubi has named audiences (`PUT /v1/server/audiences/{name} {rules}`) for mail/notifications targeting and player tags; not wired to login gating.
  - 🟡 Pragma: *Operator editing of player data* — admin storage routes can read/write/delete any object; no UI.
  - 🟡 UGS LiveOps: *Player Management console (search by player id / external id + provider; identities, state, progression, economy, moder…* — All data reachable via admin REST (`/v1/admin/accounts…`, storage, wallet, audit) but README: admin console not written yet
- **Already covered rows (5):** Metaplay: Temporary (timed) bans with reason and expiry; permanent bans; ban vi…; PlayFab: Secret key management: multiple title secret keys, create/disable/del…; PlayFab: Audit logs of Game Manager/Admin API changes (who/what/when) viewable…; PlayFab: Title Data admin: SetTitleData/SetTitleInternalData (1MB value, 1,000…; PlayFab: Audit history: Game Manager/Admin API change log with actor, timestam…

#### P0-5. Store / catalog with per-platform listings, bundles, rotation, discounts, coupons, requirements  
*area:* economy/IAP · *musubi:* 🟡 partial (20h/20p/5m over 47 rows)
- **Competitors with it (13):** AccelByte, Beamable, brainCloud, EOS, LootLocker, Metaplay, Hiro/Satori, Nakama, PlayFab, Pragma, SpacetimeDB, Steamworks, UGS LiveOps
- **Why it matters:** Monetization surface.
- **Implementation sketch (musubi terms):** Exists: `/v1/store*`, bundles, rotation, coupons, per-platform listings. Gap: flexible owned-item bundle pricing, purchase requirements (stat/entitlement/season), display pages.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Flexible pricing bundles: bundle price auto-discounted for items already owned; estimated price endpoint; order with es…* — no owned-item-aware bundle pricing
  - ❌ PlayFab: *Catalog Views (private preview): CDN-published blob snapshots of search queries, ≤10 views, ≤500MB/10k items, 1-day ref…* — No CDN snapshot export of catalog (content bundles export is operator tooling, not CDN-served)
  - ❌ PlayFab: *Deep links per platform store ({id} template, ≤10), Economy v2 settings (catalog admins, reviewers, display property ma…* — No store deep-link templating (grep deep_link: push notifications only)
  - ❌ PlayFab: *Economy v2 Game Manager tooling: Items/Bundles/Stores editors, UGC moderation queue, catalog config (admins, reviewers,…* — CLI/JSON content bundles export/import only
  - ❌ UGS LiveOps: *Webshop (hosted web store with purchases, payment providers)* — `rg -i webshop` → 0; no hosted web storefront / payment-provider checkout (only store listings + platform IAP). Sketch: Stripe/Xsolla provider in `musubi-economy` + static storefront bundle
  - 🟡 AccelByte: *Purchasing requirements: AND groups / OR between groups of conditions — entitlement owned/not owned (count), season pas…* — store listing min_level + audience targeting; no generic entitlement/stat/season predicates on listings
  - 🟡 AccelByte: *Store Display & Section: display pages with ordered sections; section types fixed / fixed-period rotation (cycle durati…* — store rotation exists (README "Store rotation"); no display/section page model, no owned-item backfill
  - 🟡 AccelByte: *Code redemption campaigns: item codes & discount codes, max redeem per code / per player / per campaign, start/end, bat…* — coupons: percent, listing-restricted, use count, once-per-account, expiry (README "Store depth"); no campaign object with batch-generated codes, per-campaign/per-player caps, redemption history or subscription-plan codes
  - 🟡 AccelByte: *Item API (49 ops): CRUD, bulk, SKU/appId lookup, item dynamic data, availability predicates, purchase conditions; Categ…* — items/listings + content bundles diff/promote; no per-change review/publish-selected workflow, no category tree
  - 🟡 Beamable: *Cloud Save (file sync)* — Storage collections w/ CAS versions, 3-way merge, blobs; no file-manifest sync SDK, no device-side offline cache (no SDK)
  - 🟡 Beamable: *Unity IAP integration + custom purchaser* — Receipt validation routes for 9 stores; no client SDK purchaser abstraction
  - 🟡 Beamable: *IAP providers: iTunes, Google Play, Steam (auth/order/prices/product), Facebook, Windows, coupon, test* — Apple/Google/Steam/Huawei/Samsung/PSN/Xbox/Nintendo/Epic validators; no Facebook/Windows Store, no begin/cancel audit trail
  - 🟡 brainCloud: *Promotions (scheduled & automated)* — store windows, `discount_percent/until`, coupons, audience-gated listings (README store) cover scheduled sales; no segment-entry-triggered personal offers with re-trigger caps
  - 🟡 brainCloud: *Store integrations setup guides* — Apple/Google/Steam/Huawei/Samsung/PSN/Xbox/Nintendo/Epic; no Amazon/Facebook/Microsoft/Meta
  - 🟡 EOS: *Ecom Interface: Epic Games Store catalog of offers → items (durable/consumable), audience items, library items; QueryOf…* — musubi validates/consumes Epic (and Apple/Google/Steam/PSN/Xbox/Nintendo/Huawei/Samsung) receipts & entitlements server-side and maps them to reward tables; it has no storefront/checkout/payment processing of its own (n…
  - 🟡 Metaplay: *In-game offers = IAP SKU + rewards + targeting; placements; offer groups; dynamic-content IAPs* — musubi store: windows, bundles, discounts, coupons, rotation, platform SKU map, store_audiences (README store row) — no placements/offer-group priority model
  - 🟡 Metaplay: *Server-side validation for Google Play, Apple App Store, Steam (server-driven purchase platform), development/fake stor…* — README IAP row: Apple/Google/Steam receipt verification + grants (`/v1/iap/*`); no Unity client flow, product catalog is `store` items not typed config
  - 🟡 Hiro/Satori: *Store purchase flow IAP* — musubi: `POST /v1/economy/store/purchase` and `/v1/economy/iap/{apple,google,...}/verify` grant store item by sku; no purchase-intent record for same-SKU disambiguation (rg "intent" -> none)
  - 🟡 Hiro/Satori: *Store types / receipt validators* — musubi: Apple/Google/Steam/Huawei/Samsung/PSN/Xbox/Nintendo/Epic IAP routes + subscriptions + App Store/RTDN notifications; no Facebook Instant, no Discord IAP (rg -i discord/fbinstant -> none)
  - 🟡 Hiro/Satori: *Externally validated purchases (RevenueCat / web store webhook)* — musubi: `POST /v1/server/economy/grant` + idempotency keys (0036_idempotency.sql) let a webhook grant; no documented external-provider pattern / RevenueCat integration
  - 🟡 Hiro/Satori: *Server-side Go interface per system (functions + lifecycle hooks: OnReward, OnClaim, OnCancel, cohort selection, store…* — musubi: WASM modules (Rust/C/Go/AssemblyScript via WIT) with `before/after` hooks on routes, `on_event`, RPC, host API (wallet/inventory/storage/notify/liveops) and typed `musubi-sim-sdk` for match logic; fewer system-s…
  - 🟡 Hiro/Satori: *Unity Purchasing v5 support, Discord embedded-app IAP, Facebook Instant purchase validation* — musubi: Apple/Google/Steam/Huawei/Samsung + PSN/Xbox/Nintendo/Epic entitlements; no Discord/FB Instant
  - 🟡 PlayFab: *SearchItems: OData-like Filter (contentType, tags, creatorId, arrays, null), OrderBy, Select, fuzzy Search on searchabl…* — musubi store/items listing with category/platform filters (`GET /v1/store`, `/v1/items`); no full-text fuzzy search or OData filters over catalog
  - 🟡 SpacetimeDB: *Subscription semantics* — `WorldDelta`/`MatchStateDelta` are per-tick atomic; storage updates are per-write, no cross-collection atomicity guarantee
  - 🟡 Steamworks: *Item store (Valve-hosted)* — store listings API (`GET /v1/store`) with windows, discounts, coupons, rotations — but no hosted storefront web UI (admin console/SDK not written)
- **Already covered rows (20):** AccelByte: Fulfillment: fulfil items on order/IAP/reward; game-server fulfilment…; Beamable: Commerce coupons (`commerce/{id}/coupons`, coupons-count) & promo cod…; brainCloud: Purchase verification per store; LootLocker: Apple App Store IAP: console IAP settings (shared secret / App Store…; LootLocker: Epic Games Store IAP: enable provider, Audience Item ID mapping, vali…; LootLocker: Microsoft Store and PlayStation Store IAP providers; Hiro/Satori: Free store items; Nakama: Apple App Store validation: StoreKit 1 (shared secret) + StoreKit 2 J…; Nakama: Samsung Galaxy Store validation + hooks (3.40); PlayFab: Bundles (multi-item purchasable, marketplace mapping), Stores (curate…; PlayFab: Legacy coupons: generate N codes per item/bundle/container in Game Ma…; PlayFab: Legacy stores: GetStoreItems (store id, segment-resolved overrides, S… …

#### P0-6. Autoscaling policies (buffer, percentage, schedules, webhook/WASM/metric rules, chain, scale-to-zero, suspend)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (11h/11p/5m over 31 rows)
- **Competitors with it (12):** Agones/OM, Beamable, Edgegap, EOS, GameLift, Hathora, Metaplay, Pragma, Rivet, SpacetimeDB, Steamworks, UGS LiveOps
- **Why it matters:** Cost vs. wait time.
- **Implementation sketch (musubi terms):** Exists: integer buffer + weekday/hour windows + EWMA forecast + scale-to-zero. Gap: percentage buffer, webhook/WASM policy, metric-threshold rules, chain/fallback, suspend/resume.
- **What the reports found weaker/missing (merged rows):**
  - ✅ Agones/OM: *Policy `Webhook`: POST `FleetAutoscaleReview{request{uid,name,namespace,status,annotations}}` every sync* — `policy.webhook {url, timeout_ms, headers}`: fleet status POSTed every reconcile, `{scale, replicas}` / `{response: {scale, replicas}}` honoured
  - ✅ Agones/OM: *Fleet autoscaler metrics: `able_to_scale`, `buffer_limits`, `buffer_size`, `current_replicas_count`, `desired_replicas_count`, `limited`* — `musubi_fleet_autoscaler_{target,desired_start,desired_stop,able_to_scale,limited,suspended,rules_firing}` + `GET …/autoscaler`
  - ❌ GameLift: *Game server groups on customer Auto Scaling groups* — no EC2 ASG / Spot integration
  - ✅ GameLift: *Rule-based autoscaling* — `policy.rules [{metric, op, threshold, for_secs, action, step, cooldown_secs}]` over fleet gauges (ready/allocated/players %, cpu/mem/tick, custom)
  - ✅ GameLift: *Suspend/resume autoscaling* — `policy.suspended`, `POST /v1/server/fleets/{id}/autoscaler/suspend|resume`, events `autoscale.suspended|resumed`
  - 🟡 Agones/OM: *`FleetAutoscaler` policy `Buffer` with `bufferSize` absolute **or percentage**, `minReplicas`, `maxReplicas`* — `buffer`, `min_servers`, `max_servers` are integers (fleet/lib.rs, mig 0050); no percentage buffer
  - 🟡 Agones/OM: *Policy `Schedule`: `between{start,end}` absolute window, `activePeriod{timezone, startCron, duration}`, nested `policy`* — `ScalePolicy.schedule[{days, from_hour, to_hour, buffer}]` (fleet/lib.rs) — weekday/hour windows in server-local time; no cron, no timezone, no absolute start/end dates, no nested policy types
  - 🟡 Agones/OM: *Graceful termination: TERM → `terminationGracePeriodSeconds`; Cluster Autoscaler honours 10 min; PDB blocks node upgrad…* — Reaper kills on failure; allocated servers are left to finish; no grace-period contract documented
  - 🟡 Agones/OM: *Metric label GC when fleets/autoscalers are deleted* — Gauges per fleet name; cleanup on fleet delete not verified
  - 🟡 Beamable: *Latency & autoscaling claims* — Measured benchmarks in README; horizontal scale via cluster nodes; no managed autoscaling of the core (operator's k8s HPA)
  - 🟡 Edgegap: *Scaling policies* — Fleet `buffer`/`min_servers`/`max_servers` + forecast/schedule policy per fleet; no metadata-filter-driven policies (e.g. "min 2 servers with map=X")
  - 🟡 EOS: *Autoscaling/capacity: fully managed, scales to Fortnite-level loads; per-user and per-deployment rate limits published…* — musubi scales horizontally (bench/soak numbers in README) and autoscale fleets, but studio operates it; rate limits configurable
  - 🟡 GameLift: *CloudWatch alarms → SNS/autoscale* — no alert rules shipped in `deploy/` (`rg -i alert deploy` → none); operators wire Alertmanager themselves
  - 🟡 GameLift: *Scaling policy object* — `ScalePolicy{forecast, horizon_min, schedule}` + buffer
  - 🟡 Hathora: *Multi-fleet with node shapes* — unlimited fleets with own spec/provider; no node-shape catalogue or node provisioning (Terraform modules provision hosts statically)
  - 🟡 Hathora: *Fleet utilization & autoscaler dashboards* — demand/usage endpoints, `matchmaker/stats`; no node-capacity dashboard data
- **Already covered rows (11):** Edgegap: Three orchestration models documented; Edgegap: Regional standby / pre-warm; Edgegap: Scale-to-zero; GameLift: Target-based autoscaling; Metaplay: Dynamic scaling of ephemeral entities (ManualShardingStrategy only):…; Metaplay: Dedicated game server fleets, autoscaling, provider integrations, geo…; Pragma: Autoscaling of backend nodes; Rivet: Production checklist: K8s manifests (2 CPU/2 GB req, 3 CPU/4 GB limit…; Rivet: Cloud autoscaling of Compute pools based on demand; Steamworks: Hosting / orchestration / autoscaling; UGS LiveOps: Autoscaling

#### P0-7. Per-server resource spec (CPU/memory/GPU limits, ports, protocols, env, secrets, grace period, restart policy)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (12h/21p/7m over 44 rows)
- **Competitors with it (11):** Agones/OM, Colyseus, Edgegap, GameLift, Hathora, Meshing vendors, Hiro/Satori, Nakama, PlayFab, Rivet, UGS Multiplayer
- **Why it matters:** Real games need more than one port and a memory cap.
- **Implementation sketch (musubi terms):** FleetSpec has `vcpu` + one port. Extend FleetSpec: `memory_mb`, `ports[{name,proto,range}]`, `env`, `secrets_ref`, `grace_secs`, `restart_policy`, provider-specific `template` passthrough.
- **What the reports found weaker/missing (merged rows):**
  - ✅ Agones/OM: *Port policy `None` (no hostPort, e.g. host networking / service mesh)* — `spec.port_policy: none` (Docker/Kubernetes publish nothing, Agones `portPolicy: None`)
  - ✅ Agones/OM: *Multiple ports per GameServer (`spec.ports[]` with name)* — `spec.ports [{name, protocol, container_port}]`, per-server host ports from the fleet range, `MUSUBI_PORT_<NAME>`, `ports.{name}` on servers/allocations
  - ✅ Agones/OM: *Port `protocol` `UDP`, `TCP`, `TCPUDP` (opens both)* — `spec.port_protocol` and per-port `protocol: udp|tcp|both`
  - ❌ Edgegap: *GPU field* — `rg -i gpu` → none
  - ✅ Edgegap: *Secret environment variables* — `spec.secret_env {K: v}` on fleets and builds: sealed with the `[security] master_keys` ring at rest (`enc:v1:…`), `***` in every fleet/build API response, decrypted only on the node starting the server and merged into the process/container env
  - ❌ GameLift: *Container group definitions* — no sidecar/per-instance group model; `rg -i sidecar` → none
  - ❌ Meshing vendors: *Memory limit* — no memory-pressure admission control (`rg memory_limit` 6 hits unrelated)
  - 🟡 Agones/OM: *Port policy `Static` (`hostPort` fixed)* — Fixed port only indirectly via `port_start == port_end`; no per-server static port field
  - 🟡 Agones/OM: *Named port **ranges** (`spec.ports[].range` referencing Helm `gameservers.portRanges`) — beta `PortRanges`* — One contiguous range per fleet; no named ranges shared across fleets
  - 🟡 Agones/OM: *Health failure strategy: crash before Ready → container restart; failing health → `Unhealthy`, never restarted, Fleet r…* — Reconciler reaps dead/unhealthy and tops the buffer back up (fleet/lib.rs); no restart-in-place semantics, no distinction between pre-Ready and post-Ready failures
  - 🟡 Agones/OM: *Limiting CPU/memory: full requests/limits on game container; SDK sidecar default CPU request 30 m, configurable limits* — `vcpu` only (fleet/lib.rs); firecracker VMs get vCPU; no memory requests/limits
  - 🟡 Agones/OM: *Game container `resources.requests/limits` default 100 m CPU request if unset* — `vcpu` only
  - 🟡 Agones/OM: *Pod `terminationGracePeriodSeconds` honoured on Shutdown* — Not documented for providers
  - 🟡 Edgegap: *Termination grace period* — `POST /v1/gameserver/shutdown` + drain exist, but no configurable SIGTERM→SIGKILL grace (`rg grace\|sigterm` → none)
  - 🟡 Edgegap: *Restart policy* — Reconciler tops up to `buffer`/`min_servers` after a crash (new server), but no per-container restart-in-place policy
  - 🟡 Edgegap: *Port mapping* — `port_start`/`port_end` range per fleet, docker provider publishes UDP+TCP; no multi-port named mappings, no per-port readiness check
  - 🟡 Edgegap: *Injected env vars* — `MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY` (README fleet row); no location/public-IP/port-map JSON, no tags
  - 🟡 Edgegap: *SIGTERM graceful stop semantics* — Drain state + shutdown route; grace period not configurable
  - 🟡 GameLift: *Container definitions* — `FleetSpec{command,image,env,working_dir,vcpu}`; health via `POST /v1/gameserver/health`; no mounts/depends-on/capabilities/essential semantics
  - 🟡 GameLift: *Connection port range & port mappings* — k8s hostPort / docker port range; server learns port via env `MUSUBI_*`
  - 🟡 GameLift: *Container discovery* — server gets `MUSUBI_SERVER_ID`, address, fleet via env
  - 🟡 Hathora: *Deployment = build + runtime config, versioned* — FleetSpec{image, command, env, working_dir, vcpu} + port range + builds (R194); no memory/CPU *limits* enforcement, no transport-type/TLS termination per port, no GPU request, no deployment tag
  - 🟡 Hathora: *Per-region cloud minimum / node baseline* — `ScalePolicy.buffer` per region + scheduled windows + forecast (R233, lib.rs) — at *server* level, not node/vCPU level; no cloud node provisioning
  - 🟡 Hathora: *Node listing & node metrics* — `GET /v1/node`, `/v1/admin/cluster/*` describe musubi nodes; fleet *hosts* are not inventoried
  - 🟡 Hiro/Satori: *Sessions: `_sessionStart/_sessionEnd`, session timeout/grace period config, session-less events extend sessions, last a…* — musubi sessions = auth sessions (JWT access/refresh, resumable WS sessions 0041), no playtime/session analytics
  - 🟡 PlayFab: *Observability: Servers overview page (search, VM/host id, session id, players, region, shutdown), archived servers + lo…* — README fleet server list + labels + Prometheus metrics; no log archive/crash dumps/VM metrics UI (BYO via k8s)
  - 🟡 Rivet: *Compute shapes 0.08–1 vCPU fractional or 2/4/8 vCPU, 128 MiB–4 GiB RAM* — fleet `tier`/resources per provider (Docker/K8s/Firecracker/Edgegap/GameLift, README L392–393) — per-fleet, not per actor
- **Already covered rows (12):** Agones/OM: Port policy `Dynamic` (allocated from `minPort–maxPort`, default 7000…; Colyseus: Graceful shutdown on SIGINT/SIGTERM/SIGUSR2/uncaughtException: `onBef…; Colyseus: Graceful shutdown on SIGTERM/SIGINT/SIGUSR2/uncaughtException: `serve…; Colyseus: Production checklist: compile TS, `NODE_ENV=production`, PM2, HTTPS r…; Colyseus: Regions: EU, NA, AS, AF, AU, SA; countries AU BR CA CL DE ES FR GB IL…; Edgegap: Environment variables; Edgegap: Deployment request (v1 `POST /v1/deploy`, v2 `POST /v2/deployments`); Hathora: Env var validation; Hathora: Pick fleet per deployment (multi-fleet); Nakama: Edgegap integration guide (Edgegap Nakama plugin, deployment env vars…; Rivet: Graceful shutdown: SIGTERM drain, K8s `terminationGracePeriodSeconds`…; Rivet: Env vars: `RIVET_ENDPOINT`, `RIVET_TOKEN`, `RIVET_NAMESPACE`, `RIVET_…

#### P0-8. Bans (account, timed, shadow, device/IP/MAC, feature bans, reason catalog, ban sync)  
*area:* moderation/anti-cheat · *musubi:* 🟡 partial (9h/11p/5m over 25 rows)
- **Competitors with it (11):** AccelByte, Beamable, Colyseus, EOS, Hathora, LootLocker, Nakama, Photon, PlayFab, Pragma, Steamworks
- **Why it matters:** T&S baseline.
- **Implementation sketch (musubi terms):** Exists: timed/shadow/message bans, kick, group bans. Gap: device/IP/HWID bans with device registry, feature-level bans, ban-reason catalog.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Devices V4: list all devices a user ever logged in from, banned devices list, device bans per user, ban/unban device; d…* — no device registry / device bans (rg `device_ban` none)
  - ❌ EOS: *Easy Anti-Cheat (Anti-Cheat Client + Server Interfaces): protected game client mode via bootstrapper (start_protected_g…* — musubi has no client anti-cheat; only /v1/server/anticheat/flag\|verify signal API + EAC/BattlEye ticket relay + WASM host anticheat API (partial glue only)
  - ❌ PlayFab: *Segment actions: on enter/exit — grant VC, grant item, send push, send email, execute CloudScript, ban, delete player,…* — No triggered actions on audience membership change (grep "segment": only migrate-playfab)
  - ❌ Pragma: *Portal: Bans page* — API only.
  - 🟡 Beamable: *Admin feature (player admin, credentials, bans) & Microservices Manager overview pages* — Admin API only
  - 🟡 LootLocker: *Ban error contract: `player_banned` 403 at session start with sanitized `ban` object (reason manual/chargeback, banned_…* — Login of banned account returns 403 with reason/expiry; no chargeback-originated bans, no tokenless ban-status probe
  - 🟡 LootLocker: *Player Name Blacklist (Aug 2025): console-managed banned names for player names* — Chat word filters (`/v1/admin/chat/filters`) exist; display-name policy only uniqueness/length (rg blacklist\|profanity = 0)
  - 🟡 PlayFab: *Player Toolbar / Players page in Game Manager: search by PlayFabId/display name/email/username/linked id, player detail…* — musubi: admin HTTP API (`/v1/admin/accounts` search by identity, patch, bans, GDPR export) + CLI — no console UI
  - 🟡 PlayFab: *Ban system: BanUsers (reason, duration, IP, MAC, UserFamilyType), GetUserBans, RevokeBans/RevokeAllBansForUser, UpdateB…* — README bans (duration, reason, notes) `/v1/admin/accounts/{id}/ban` + device-level lockout; no IP/MAC/family bans (grep ip_ban: none)
  - 🟡 Pragma: *Social backend / Game backend split* — musubi is one binary / one Postgres per deployment (README "Layout"); multi-title is not a first-class concept (no shared social cluster across titles; `tenant` only appears for Azure AD).
  - 🟡 Pragma: *Bans: scopes* — account bans with expiry + reason + kick (`/v1/admin/accounts/{id}/ban`, realtime kick); single-title so no per-game scope; ban history list exists in audit log only.
  - 🟡 Pragma: *Ban reference: fields* — ban row has reason/expiry/by-key; no scope or propagation fields.
  - 🟡 Steamworks: *Player bans lookup* — musubi exposes its own bans (`GET /v1/admin/accounts/{id}/ban`, anti-cheat flags) but cannot read VAC/community/economy ban state of Steam users
  - 🟡 Steamworks: *VAC* — EAC/BattlEye ticket verification + own anti-cheat signals (server flags, module flags, score windows, auto kicks) — no first-party cheat detection client
  - 🟡 Steamworks: *Players banned feed* — bans are per account only (`GET /v1/admin/accounts/{id}/ban`, `account.banned` event/webhook); no paged list of all banned accounts in ROUTES
- **Already covered rows (9):** AccelByte: Reporting & moderation: report categories (user/chat/UGC/extension),…; Colyseus: Auth integration: auto-mounted routes, `onCheckBanned` `{reason, unti…; Hathora: Bans, reports, moderation, login queue, idempotency keys, resumable s…; LootLocker: Ban player: optional expiry, player-facing ban detail message, intern…; Nakama: Ban user (console/runtime `usersBanId`/`usersUnbanId`); banned users…; Photon: Hacking protection; PlayFab: Player ban system: BanUsers (duration/permanent, reason, by IP/MAC),…; Pragma: Ban notification to client; Steamworks: Game Bans (publisher-issued)

#### P0-9. Game server log capture (stdout/stderr tail, download, retention, signed URLs)  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (2h/4p/6m over 14 rows)
- **Competitors with it (10):** Beamable, Edgegap, GameLift, Hathora, Meshing vendors, Metaplay, Nakama, PlayFab, Steamworks, UGS Multiplayer
- **Why it matters:** Debugging live servers without SSH.
- **Implementation sketch (musubi terms):** Missing. Add log shipper in providers (docker/k8s/ssh) → object store + `GET /v1/server/gameservers/{id}/logs?follow` on musubi-fleet.
- **What the reports found weaker/missing (merged rows):**
  - ✅ Edgegap: *Endpoint-storage link per version (log shipping + pull profiles)* — `spec.logs.archive` ships terminated servers' captured/SDK lines to the S3-compatible store as gzip NDJSON (`musubi.server_log_archives`, `GET /v1/server/fleets/{id}/logs/archives/{aid}`); pull profiles n/a (no build artifacts pulled by fleet)
  - ❌ Edgegap: *Container logs (live + after stop)* — No stdout/stderr capture of game servers (`rg container_log` → none)
  - ❌ GameLift: *Game session log URL* — no game-server log capture/upload
  - ❌ Hathora: *Live log tailing & download (Console/API/CLI `hathora log`)* — no game-server log capture in `musubi-fleet`; musubi node logs via `tracing`/journald only
  - ❌ UGS Multiplayer: *Log & crash-dump collection UI* — No server log/crash-dump capture (`rg -i 'crash.?dump'` → 0).
  - 🟡 GameLift: *Container logs* — provider logs (docker/k8s) outside musubi; no log-destination config
  - 🟡 Meshing vendors: *Replication-server logs per room* — fleet server logs via provider; no UI
  - 🟡 Metaplay: *Custom server logic in C# (same language as client, no sandbox)* — WASM modules (Rust/any wasm lang) with hot reload/canary — different trade-off
  - 🟡 Steamworks: *Game Server Login Tokens (GSLT)* — server keys (`POST /v1/admin/keys role=server`) identify fleets, not individual community servers; no per-server persistent identity/ban that follows a token
- **Already covered rows (2):** Nakama: Logger: level, stdout/file, rotation (`max_size`, `max_age`, `max_bac…; Steamworks: Game server login to Steam

#### P0-10. Hosting providers (Kubernetes/Agones, bare-metal SSH, Firecracker, cloud VMs, Edgegap/GameLift/Hathora adapters)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (14h/19p/8m over 60 rows)
- **Competitors with it (10):** AccelByte, Agones/OM, Edgegap, GameLift, Hathora, Metaplay, Photon, Pragma, SpacetimeDB, UGS Multiplayer
- **Why it matters:** Studios have different infra constraints.
- **Implementation sketch (musubi terms):** Exists: 9 providers (local/docker/k8s/agones/firecracker/ssh/webhook/edgegap/gamelift).
- **What the reports found weaker/missing (merged rows):**
  - ✅ Agones/OM: *`spec.eviction.safe` = `Never` / `OnUpgrade` / `Always` controls Cluster Autoscaler + node-upgrade disruption* — `spec.kubernetes.safe_to_evict` → CA `safe-to-evict` annotation on Pods, `spec.eviction.safe` in Agones mode
  - ❌ Agones/OM: *Feature gates with Alpha/Beta/Stable stages (`agones.featureGates`), semver policy, deprecation path (e.g. PlayerTracki…* — No feature-flag mechanism or published stability tiers; README says "alpha" globally
  - ❌ Agones/OM: *KubeStellar Console (multi-cluster dashboard with fleet monitoring), Minikube Agones cluster automation* — Admin console not written (README:status)
  - ❌ Agones/OM: *`agones.allocator.tlsSecret`/`clientCASecret`/`generateTLS`/`generateClientTLS`/`disableMTLS`/`disableTLS`/`disableSecr…* — No built-in TLS/mTLS management
  - ❌ Agones/OM: *`agones.dev/sdk-*` labels set by the game can be matched by allocation selectors (label locking)* — Labels not selectable
  - ✅ GameLift: *Server logs (EC2)* — stdout/stderr/SDK lines stored, archived to object storage on termination or on demand (`POST …/servers/{sid}/logs/archive`), downloadable raw or decoded
  - ❌ Hathora: *GPU compute* — `rg gpu` → 0 hits
  - 🟡 Agones/OM: *Declarative `GameServer` custom resource managed through the Kubernetes API (`kubectl apply/get/describe`, events, RBAC)* — Game servers are rows in `musubi.game_servers` (mig 0010) driven by `GET/DELETE /v1/server/gameservers/{id}` and the fleet reconciler; no declarative object model, no kubectl/RBAC integration, no per-object event stream…
  - 🟡 Agones/OM: *`agones.dev/last-allocated` RFC3339 annotation set on every allocation* — `allocation` JSON column is overwritten on allocate (fleets.rs) and `version` increments; no explicit last-allocated timestamp field documented
  - 🟡 Agones/OM: *Local dev `GameServer` via annotation `agones.dev/dev-address` (no Pod created; developer runs the binary)* — `local` provider spawns the binary on the node, `musubi dev`; no "register an external process by address" path
  - 🟡 Agones/OM: *`agones-allocator` service: gRPC **and** REST on one or two ports, `LoadBalancer` Service with reserved IP, mTLS client…* — Allocation is plain HTTPS with a server key (`Authorization: Bearer`); no gRPC, no mTLS client certificates (`rg -i mtls` → 0); TLS is terminated by the ingress/LB
  - 🟡 Agones/OM: *Allocation metrics: `agones_gameserver_allocations_duration_seconds` (histogram), `allocations_retry_total`, `allocatio…* — `musubi_http_request_duration_seconds{route}` covers the allocate route; no per-fleet allocation counter or retry metric
  - 🟡 Agones/OM: *Helm chart `agones/agones` with ~150 values (`agones.featureGates`, `rbacEnabled`, `registerWebhooks`, `registerApiServ…* — `deploy/helm/musubi` (ConfigMap TOML, `MUSUBI__*` secrets, Service HTTP+UDP, HPA, PDB, serviceaccount) — far fewer knobs, no values schema
  - 🟡 Agones/OM: *`agones.crds.cleanupOnDelete` pre-delete hook deletes all GameServers/Pods on chart uninstall* — `DELETE /v1/server/fleets/{id}` terminates its servers; no uninstall hook
  - 🟡 Agones/OM: *Go clientset `agones.dev/agones/pkg/client/clientset/versioned` for CRDs; any k8s client; `kubectl`* — HTTP/JSON with `GET /openapi.json` for codegen (README:status); no published typed client
  - 🟡 Agones/OM: *Custom controller example that reacts to Agones CRs (informers)* — Webhooks (`/v1/admin/webhooks`) and `GET /v1/server/events` for reacting to events; no informer-style watch of all servers
  - 🟡 Agones/OM: *`agones.allocator.service.*`: `serviceType`, `clusterIP`, `loadBalancerIP`, `loadBalancerSourceRanges`, `externalTraffi…* — Helm `service.yaml` exposes HTTP + UDP edge; fewer knobs
  - 🟡 Agones/OM: *`agones.allocator.pdb.*`, `topologySpreadConstraints`, `nodeSelector/tolerations/affinity`, `updateStrategy`, `env`* — Single Deployment with PDB/HPA
  - 🟡 Agones/OM: *`agones.gameservers.minPort` / `maxPort` (7000–8000), `portRanges{name:{minPort,maxPort}}`, `namespaces[]`, `podPreserv…* — Port range per fleet; no global ranges or namespaces list
  - 🟡 Agones/OM: *`agones.image.*` per component (registry, tag, pullPolicy, pullSecret, sdk cpu/memory requests+limits, `sdk.securityCon…* — One image; `values.yaml` image/tag/pullPolicy
  - 🟡 GameLift: *Managed EC2 fleets* — `crates/musubi-fleet` orchestrates processes on docker/k8s/agones/firecracker/ssh/edgegap/gamelift providers; no cloud-instance lifecycle (no AMI/instance-type abstraction, no OS patching)
  - 🟡 GameLift: *Managed container fleets* — `kubernetes`, `agones`, `docker` providers run images (`FleetSpec.image`) but no GameLift-style container-group abstraction
  - 🟡 GameLift: *Capacity limits per location* — `min_servers/max_servers/buffer`; `GET …/servers` states ready/allocated/draining
  - 🟡 Hathora: *Autoscaler semantics* — musubi autoscaling is ready-pool buffer + allocation-history forecast + weekday/hour schedule (ScalePolicy); no utilization-threshold node autoscaler
  - 🟡 Hathora: *Hybrid bare metal + cloud burst* — Hetzner/OVH Terraform + ssh fleet hosts + docker/k8s providers can mix; no automatic burst-to-cloud node provisioning
  - 🟡 UGS Multiplayer: *Managed fleet of game servers (bare metal + cloud)* — `Fleet{provider, region, pool, spec, buffer, min/max_servers, policy, port range}` with providers local/docker/firecracker/kubernetes/agones/edgegap/gamelift/ssh/webhook (`crates/musubi-fleet/src`). One provider+region…
- **Already covered rows (14):** AccelByte: AMS (AccelByte Multiplayer Servers): fleets per region, production vs…; AccelByte: Regions: AWS-hosted control plane; AMS DS regions across AWS/Azure/GC…; Agones/OM: Port policy `Passthrough` (containerPort == hostPort, chosen by Agone…; Agones/OM: `agones-allocator` runs 3 replicas by default; HA for allocation; Agones/OM: Integration pattern: allocate from a Fleet by `agones.dev/fleet` label; Agones/OM: Control plane split: `agones-controller` (reconcile loops, leader ele…; Agones/OM: `agones-ping` HTTP service returning a configurable string (identify…; Agones/OM: Agones ↔ Open Match reference flow: Director fetches matches, calls `…; Agones/OM: Agones: one cluster per region + multi-cluster allocation policies; O…; Edgegap: Firecracker microVM servers; Edgegap: Bare-metal via SSH; Photon: Hosting providers list …

#### P0-11. CLI (dev loop, deploy, logs, doctor, config, module/fleet management, troubleshooting)  
*area:* deployment/ops · *musubi:* 🟡 partial (8h/13p/2m over 27 rows)
- **Competitors with it (10):** AccelByte, Beamable, brainCloud, Hathora, Meshing vendors, Metaplay, Pragma, Rivet, SpacetimeDB, UGS LiveOps
- **Why it matters:** Developer productivity.
- **Implementation sketch (musubi terms):** Exists: musubi-cli. Add `musubi logs --follow`, `musubi init`, `musubi mcp`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *AGS API MCP Server (hosted), AGS CLI MCP, Extend SDK MCP — AI assistant integration* — no MCP server
  - ❌ Beamable: *MCP server + AI skills (`beam mcp setup`, `beam install-ai-skill`) (CLI 7.2.0)* — None
  - 🟡 Beamable: *CLI output modes* — musubi CLI prints JSON for some commands; no uniform `--raw` envelope
  - 🟡 Beamable: *CLI listen to notifications (diagnostic)* — `musubi bench --scenario ws` and dev fixtures; no event tail command (grep)
  - 🟡 Beamable: *Local Grafana for telemetry (`beam grafana open/ps/stop`)* — Prometheus metrics + Helm (Grafana dashboards not shipped; grep grafana = 0)
  - 🟡 Beamable: *Content CLI: tags, history, snapshots, manifests* — Content bundles versions/diff/rollback via API; no tags, no local-vs-remote status/conflict CLI
  - 🟡 Beamable: *Org/realm mgmt from CLI (`org new/games/realms`, `config realm set/remove`)* — Projects via admin API; no org concept
  - 🟡 Beamable: *`beam listen player/server` & `beam server clear`* — No tail command
  - 🟡 Metaplay: *Rolling back a release: dashboard config rollback, server image rollback via Metaplay Portal/CLI (`metaplay deploy serv…* — `musubi module rollback`, content rollback; image rollback = plain Helm/K8s
  - 🟡 Metaplay: *Observability stack per environment: Grafana + Prometheus(Mimir) + Loki, default `Metaplay Server` dashboard, custom Al…* — Prometheus `/metrics` + OpenTelemetry traces (README observability row); no shipped Grafana dashboard or alert rules (`rg -il 'grafana\|alert' deploy/` → none)
  - 🟡 Metaplay: *Metaplay CLI (Go, Apache-2.0, github.com/metaplay/cli): auth (login/logout/machine-login/whoami), build (botclient/dash…* — `musubi-cli`: dev, module (publish/canary/promote/rollback/versions), fleet push/list, schema, idl, deploy, migrate, migrate-nakama/-playfab, export/import, doctor, config, bench, soak (crates/musubi-cli/src/main.rs); n…
  - 🟡 Rivet: *CLI `@rivetkit/cli`: `dev`, `deploy`, `pool list\|delete`, `engine`, `setup-ci` (GitHub Actions), `token create`, `logs`* — `musubi dev/module/fleet/schema/idl/deploy/migrate/migrate-nakama/migrate-playfab/export/import/doctor/config/bench/soak` — broader, but no `setup-ci`, `logs`, `token` commands
  - 🟡 SpacetimeDB: *Multi-server CLI config* — `musubi config` + `MUSUBI_URL`-style flags; single target per invocation
  - 🟡 UGS LiveOps: *Project-level resource policy (Allow/Deny statements for Player principal on `urn:ugs:<service>:<path>` with `Read`/`Wr…* — Per-route auth tiers (public / session / server key) + storage collection access policies and permission codes; no generic per-service URN policy language — sketch: route-pattern ACL table evaluated in the axum auth lay…
  - 🟡 UGS LiveOps: *UGS CLI (`ugs`) — 1,219 doc pages: auth, deploy/fetch, per-service commands, access, scheduler, triggers, DLQ, player m…* — `musubi` CLI: serve/dev/migrate/export/import/deploy/module/schema/doctor/bench/soak/transfer (`crates/musubi-cli/src/`); fewer per-subsystem CRUD commands (REST instead)
- **Already covered rows (8):** AccelByte: AGS CLI (`ags`): every API endpoint as a command (describe/JSON/no-in…; Beamable: Beam CLI (.NET tool) & workspace; brainCloud: Builder API + CLI; Hathora: `hathora` Go CLI; Meshing vendors: Worker flags; Meshing vendors: CLI; Rivet: Benchmarks & soak harness (`bench`, `soak`, nightly GitHub Actions); UGS LiveOps: Queries with secondary indexes

#### P0-12. Subscriptions (renewals, grace, server notifications, status)  
*area:* economy/IAP · *musubi:* 🟡 partial (4h/9p/3m over 18 rows)
- **Competitors with it (10):** Beamable, brainCloud, Colyseus, LootLocker, Metaplay, Nakama, Photon, Rivet, SpacetimeDB, Steamworks
- **Why it matters:** Battle-pass-as-subscription, VIP.
- **Implementation sketch (musubi terms):** Exists: `/v1/iap/subscriptions` + Apple/Google notifications.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Stripe web checkout: Stripe Connect account (manual onboarding), publishable key, success/cancel URLs, Live/Test mode,…* — rg -i stripe = 1 incidental hit; no web payment provider / checkout sessions; subscriptions only via platform IAP (`/v1/iap/subscriptions`)
  - ❌ LootLocker: *Stripe subscription cancel (`cancel-stripe-subscription`), async purchase init (`initiate-async-purchase`), checkout se…* — No Stripe
  - 🟡 Beamable: *Stats subscription (`PUT /basic/stats/subscribe`) & client-side `Subscribe`* — Storage subscriptions exist; player-stats have no dedicated live subscription (grep)
  - 🟡 Metaplay: *Auto-renewing subscriptions: Apple/Google server notifications, state tracking, reuse across accounts handling, subscri…* — README IAP row: product `kind: "subscription"` + `grace_secs`, Apple transaction id / Google `purchases.subscriptionsv2` lookup, re-grant per period, plus `/v1/entitlements` sync; no store server-notification ingestion…
  - 🟡 Nakama: *Purchase history: `purchasesList`, `purchaseGetByTransactionId`, console Purchases/Subscriptions views with filters (3.…* — `GET /v1/iap/purchases`, admin purchase lookup routes; no console UI
  - 🟡 Rivet: *`canSubscribe` per-event authorization hook* — channel/party/match membership gates events; no per-event ACL hook inside a module
  - 🟡 SpacetimeDB: *Query subscriptions (live row replication)* — `StorageSubscribe/StorageUpdate` (collection-level), `WorldSubscribe/WorldDelta` (interest-managed entity deltas), `StatusFollow`; no arbitrary filtered query subscriptions
  - 🟡 SpacetimeDB: *Subscription handles* — `StorageUnsubscribe`/`WorldUnsubscribe` exist; no applied/error callbacks contract
  - 🟡 Steamworks: *Subscriptions & downloads* — `POST /v1/ugc/{id}/subscribe`, list subscriptions; download is a plain blob GET — no client-side install/load-order management (SDK)
  - 🟡 Steamworks: *Queries & ranking* — `GET /v1/ugc?kind&tags&q&sort=rating\|recent\|popular` (README UGC search); no trend-over-N-days, playtime-based ranks, friends-favourites or collections
  - 🟡 Steamworks: *Item statistics* — counts for ratings/subscriptions/downloads; no playtime/follower stats
- **Already covered rows (4):** brainCloud: Subscriptions (recurring); Colyseus: Xsolla integration guide (merchant of record): payment token for Virt…; Nakama: Google Play validation via service account (`iap.google.client_email/…; Nakama: Subscriptions: `ValidateSubscriptionApple/Google`, `ListSubscriptions…

#### P0-13. Placement: latency/region/geo selection, ping beacons, QoS measurement, multi-region fallback  
*area:* matchmaking · *musubi:* 🟡 partial (11h/12p/3m over 26 rows)
- **Competitors with it (10):** AccelByte, brainCloud, Colyseus, Edgegap, GameLift, Metaplay, Photon, PlayFab, Rivet, UGS Multiplayer
- **Why it matters:** Low ping is retention.
- **Implementation sketch (musubi terms):** Exists: `GET /v1/regions` + UDP echo + `country_map`. Gap: global beacon network, geo-coordinate radius fallback, packet-loss dimension, client QoS SDK.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *`require_cached_locations` placement flag* — No notion of image locality in placement
  - ✅ Edgegap: *Placement by geo coordinates* — `POST /v1/server/allocate {pool, regions[], pings: {region: ms}, max_ping_ms, client: {lat, lon}, region}`: explicit preference → lowest ping (ceiling drops regions) → nearest region by `[fleet.regions.<name>] lat, lon` → hint; allocation carries `route {region, reason, ping_ms, distance_km, rank, tried}`
  - ✅ GameLift: *Placement object* — `POST /v1/server/session_placements` queues a routed allocation with timeout/metadata, leader worker retries until fulfilled/cancelled/timed_out, `GET/DELETE /v1/server/session_placements/{id}`, `placement.*` events
  - 🟡 Colyseus: *`selectProcessIdToCreateRoom(roomName, options)` — custom placement, default = process with fewest rooms (alt: fewest C…* — matches homed on leader / node regions; worlds have placement strategies (adjacency/points_of_interest/player_density/custom picker); no per-room-type custom placement callback for matches
  - 🟡 Edgegap: *Placement by player IPs ("Server Score": latency + fairness via telemetry)* — Georouting picks the fleet region with lowest worst client ping from client-measured `ping_<region>` ticket props (README `GET /v1/regions`); no server-side IP→latency telemetry or fairness model
  - 🟡 Edgegap: *Placement filters* — Explicit `region` only
  - 🟡 Edgegap: *Session API (v1)* — `POST /v1/server/allocate {backfill:true}` reuses servers with free capacity (`players`/`capacity`), cold-start `wait_secs`; no socket-typed sessions, no selectors, no per-session webhook
  - 🟡 GameLift: *Placement events* — `match.started`/`match.ended` webhooks; no placement-level events
  - 🟡 GameLift: *Queue metrics* — matchmaker wait p50/p95/p99 per queue/region (`/v1/server/matchmaker/stats`), fleet demand; no placement success/first-choice metrics
  - 🟡 GameLift: *Event-based placement sample* — webhooks with HMAC/retry/DLQ
  - 🟡 GameLift: *Player latency in placement* — ticket `ping_<region>` props in matchmaking only
  - 🟡 Metaplay: *Sharding strategies: SingletonService, DynamicService, MultiService(n), StaticSharded, Manual; `EntityShardGroup` init…* — musubi-cluster: node roles (edge/world/match/…), Raft node bus, leases, placement by role (README cluster row); no per-entity-kind sharding strategy catalogue
  - 🟡 Photon: *Ping-based region selection cache* — server-side ping tables per fleet; client cache is SDK work
  - 🟡 PlayFab: *QoS & regions: ListQosServersForTitle (RoutingPreference), QoS beacon UDP ping, ListPartyQosServers, EnableMultiplayerS…* — `GET /v1/regions` probes; no quota-change workflow (n/a for self-host) → counted partial for QoS beacon parity (HTTP/echo probes vs dedicated UDP beacons)
  - 🟡 UGS Multiplayer: *QoS-aware rules* — `ping_<region>` numeric props + queue ping windows + region pick by lowest worst ping (README "Regions"); no packet-loss dimension.
- **Already covered rows (11):** AccelByte: Region/latency: client supplies latencies map from QoS servers per re…; brainCloud: Hosting regions; brainCloud: Ping-based region selection; GameLift: UDP ping beacons; GameLift: Desired player sessions; GameLift: Standalone mode (bring your own hosting); GameLift: Standalone placement service guidance; PlayFab: Ticket attributes: DataObject or EscapedDataObject per user, Latencie…; Rivet: Regions atl/bom/fra/gru/lax/osa/sin/syd with automatic region selecti…; UGS Multiplayer: QoS Discovery API; UGS Multiplayer: QoS discovery + UDP QoS servers per fleet

#### P0-14. Server-side scripting runtimes (WASM, JS/TS, Lua, C#/.NET, Go, Kotlin, Python)  
*area:* runtime/extensibility · *musubi:* 🟡 partial (15h/22p/5m over 53 rows)
- **Competitors with it (10):** AccelByte, Beamable, brainCloud, Metaplay, Nakama, PlayFab, Pragma, Rivet, SpacetimeDB, UGS LiveOps
- **Why it matters:** Language choice is the #1 adoption blocker for WASM-only.
- **Implementation sketch (musubi terms):** WASM Component only today. Add JS/TS path: compile TS → WASM via `componentize-js`/Javy in `musubi module build`; publish TS host-API types. C#/.NET via NativeAOT-LLVM wasm later.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Extend App UI: custom admin pages embedded in Admin Portal (hosted assets)* — no admin console at all
  - ❌ brainCloud: *VS Code extension & MCP server* — no editor extension / MCP server (CLI only)
  - ❌ SpacetimeDB: *C# modules* — WASM modules must be built with musubi bindings (Rust/any wasm32 toolchain, no C# SDK)
  - 🟡 Beamable: *C# Microservices (.NET, Docker)* — WASM modules (Rust/any WIT lang) with rpc/hooks/schedules — not C#/.NET, no engine client codegen (bindings.rs only)
  - 🟡 Beamable: *Microservice remote logging & log config rules* — Module logs via host API + tracing; no per-player dynamic log-level rules
  - 🟡 Beamable: *Microservice request context & client headers* — Module rpc context has account/session; no standard client-version headers helpers (no SDK)
  - 🟡 Beamable: *Portal microservice management* — module versions/canary/rollback via CLI + Prometheus; no UI, no per-module metrics page
  - 🟡 Beamable: *Standalone microservice csproj properties* — module manifest (name, hooks, limits); no log provider choice, no engine client codegen
  - 🟡 Beamable: *Federation types* — Hooks: before/after auth, inventory, match hooks, fleet `webhook` provider ≈ game-server federation; no player-init federation hook (grep hook_account_created)
  - 🟡 Beamable: *Microservice routing (`/basic/{cid}.{pid}.micro_{service}/{method}`)* — `/v1/rpc/{module}/{fn}`; canary by account list ≈ routing to a version, but no per-developer local routing key
  - 🟡 Beamable: *Microservice analytics emit* — Host API can emit analytics events to bus (grep `analytics` host fn) but no warehouse
  - 🟡 Beamable: *Microservice OpenAPI docs page per service (`/docs`, `project open-swagger`)* — Core OpenAPI; module RPCs not auto-documented
  - 🟡 brainCloud: *Logging service (Cloud Code → Recent Errors)* — WASM `log` host fn + tracing/OTel; no per-app error-log browser (console missing)
  - 🟡 brainCloud: *Scripts (JavaScript on Rhino, ES6)* — WASM modules (Rust/any language) with `rpc`, versions/canary/rollback, hot reload (`musubi-runtime`); **no JavaScript/Lua scripting, no in-browser editor**
  - 🟡 Metaplay: *.NET unit testing helpers (NUnit, `Backend/SharedCode.Tests`, execute PlayerActions against PlayerModel offline)* — musubi-sim-sdk + fixtures (`/v1/admin/dev/fixtures`) for WASM logic tests; no shared-logic unit harness
  - 🟡 Metaplay: *System tests with Playwright.NET (`Backend/System.Tests`, `MetaPageTest`, simulated clients – limited)* — request recording/replay (`/v1/admin/dev/record`, README dev tooling) covers API-level; no browser/dashboard tests (no dashboard)
  - 🟡 Nakama: *Embedded server framework: Go plugins (`.so`, full stdlib, exact dependency pinning, platform-matched), Lua 5.1 (gopher…* — musubi runtime = WASM (wasmtime, Component Model/WIT, `cargo build --target wasm32-wasip1`) — any language compiling to WASM, but no Lua/JS scripting path and no Go-native plugin; smaller host API surface (`musubi-runti…
  - 🟡 Nakama: *`NakamaModule` API (149 fns): authenticate*/link*/unlink*, `AuthenticateTokenGenerate` (custom JWT w/ vars), accounts g…* — musubi `host_call` ops cover storage/economy/leaderboards/notifications/chat/liveops/metrics/http (rg host_call); no raw SQL, no stream primitives, no NotificationSendAll broadcast equivalent verified (rg -i send_all →…
  - 🟡 Nakama: *Match handler contract: `MatchInit` (params, returns state/tickRate/label), `MatchJoinAttempt` (accept/reject + reason)…* — musubi actor runtime: WASM match module with `join_attempt/join/leave/tick/input` + snapshot/journal, failover, rollback, record/replay, spectators (README rows 208–209) — comparable lifecycle but only via WASM
  - 🟡 Nakama: *`AuthenticateTokenGenerate` (mint session token for arbitrary user id with custom expiry/vars), `UsersGetRandom`, `Grou…* — musubi WASM host ops: http fetch, storage, economy, chat, notifications, liveops, metrics, logging (rg host_call); no raw SQL, no token-minting op, no localcache (WASM instance memory instead)
  - 🟡 PlayFab: *Profiles API: GetProfile(s), SetProfileLanguage, SetProfilePolicy (entity-level ACL statements: resource/action/princip…* — musubi storage permission codes + access policies per collection (README storage policies), server-key roles; no per-entity ACL statements for arbitrary API actions
  - 🟡 SpacetimeDB: *Rust modules* — musubi WASM modules (Rust via `musubi-sim-sdk` bindings) — but API is host-call JSON, not typed table access
  - 🟡 SpacetimeDB: *Module ABI spec* — host ABI (`abi::host`, JSON ops) documented in README, not a formal spec
  - 🟡 UGS LiveOps: *JavaScript scripts (Node runtime)* — Server logic is WASM modules (`musubi-runtime`, any language compiling to wasm32-wasip1/Component Model, Rust bindings generated from IDL). No JavaScript runtime — a JS team must compile JS→WASM (e.g. Javy) themselves
  - 🟡 UGS LiveOps: *C# modules (.NET 7/8/9)* — WASM modules with fuel/memory/wall-clock limits (`[runtime]`), no cold starts (resident in process), typed bindings via `GET /v1/admin/idl?format=bindings`; no C# toolchain/templates (C# → WASM via NativeAOT-LLVM is pos…
- **Already covered rows (15):** AccelByte: Configuration Migration: export selected services from source env/nam…; AccelByte: Profanity filter: default profane-word DB, custom filters via Extend…; AccelByte: Extend Service Extension: custom REST/gRPC services hosted by AGS (Go…; Beamable: Scheduled jobs; Beamable: Microservice access attributes; Beamable: Microservice hot reload & IDE debugging; Beamable: Microservice `Requester` (admin-privileged platform calls) & `Service…; Beamable: Microservice 10 s hard timeout / 504; Beamable: Event subscriptions from microservices (legacy event topics); Beamable: History / audit endpoints; brainCloud: WebHooks (inbound HTTP → script); Pragma: Typed Kotlin config + YAML overlays …

#### P0-15. Generic OIDC / OAuth2 provider catalog  
*area:* auth · *musubi:* 🟡 partial (8h/6p/2m over 17 rows)
- **Competitors with it (9):** AccelByte, EOS, LootLocker, Metaplay, PlayFab, Pragma, SpacetimeDB, Steamworks, UGS LiveOps
- **Why it matters:** Lets studios plug any IdP without code.
- **Implementation sketch (musubi terms):** Exists: `POST /v1/auth/oidc/{provider}` (id_token). Gap: authorization-code flow with redirect handling + provider catalog like Colyseus `grant`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Auth Client API v3 (auth.game): Config, External Authentication, List Player Identity Providers, OAuth Authorize/Token/…* — musubi is OIDC *client* only; no OAuth authorization server / hosted login for third parties (rg 'authorize' = token exchange only)
  - ❌ UGS LiveOps: *Native C/C++ SDK (non-Unity engines)* — No native client library; REST/WS only (`GET /openapi.json` for generators)
  - 🟡 AccelByte: *Admin SSO via enterprise IdP (Azure AD SAML, Google Workspace, OIDC)* — operator SSO via OIDC only (`/v1/admin/sso`); no SAML
  - 🟡 AccelByte: *IAM clients (OAuth2 confidential/public, client credentials for DS/uploaders, per-client permissions, redirect URIs)* — server keys with scopes (`/v1/admin/server-keys`); no OAuth2 client model / public clients / redirect URIs
  - 🟡 Metaplay: *Player details page: overview card (name, level, last login, country, logic version, developer flag), Dynamic Data card…* — all of these exist as `/v1/admin/*` and `/v1/server/*` HTTP ops (accounts, bans, mail, rewards, audit, export) but no UI
  - 🟡 PlayFab: *API access policy (title-level JSON policy: allow/deny per API per principal, comment, Apply via Admin/UpdatePolicy)* — musubi: per-collection access policies via IDL/CEL-like rules and server-key roles; no generic per-route allow/deny policy document
  - 🟡 Pragma: *Okta / Auth0 / Google Workspace (operator SSO)* — any OIDC issuer works for players (`[auth.oidc.*]`), but operators cannot log in through an IdP.
  - 🟡 SpacetimeDB: *SpacetimeAuth managed IdP (beta, Maincloud)* — musubi is itself the IdP (email/password, magic link `POST /v1/auth/email/magic/*`, device, OIDC, passkeys `0092`, TOTP `0093`), with JWKS at `/.well-known/jwks.json`; no hosted login UI/theme, no multi-project auth ten…
- **Already covered rows (8):** EOS: ID Tokens (OIDC JWT) with claims appid, aud, cty (country), dn, pfdid…; EOS: Proprietary account system via OpenID Connect identity provider (conf…; LootLocker: Sign in with Google (OAuth2 id token, refresh); PlayFab: LoginWithOpenIdConnect (connection id, id token) with Admin CreateOpe…; Pragma: Custom identity provider plugin; SpacetimeDB: OIDC token auth (bring your own IdP); SpacetimeDB: Service-to-service auth; UGS LiveOps: OpenID Connect custom provider

#### P0-16. Self-hosting packaging (single binary, Docker image, docker-compose, Helm chart, Terraform, PaaS one-click)  
*area:* deployment/ops · *musubi:* 🟡 partial (10h/11p/3m over 28 rows)
- **Competitors with it (9):** Agones/OM, Beamable, Colyseus, Edgegap, Metaplay, Nakama, Rivet, SpacetimeDB, Steamworks
- **Why it matters:** Low-friction installs win OSS adoption.
- **Implementation sketch (musubi terms):** Exists: Dockerfile, Helm, Terraform (k8s/Hetzner/OVH). Add docker-compose, multi-arch images, Railway/Render/Coolify templates.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *Helm `values.schema.json` validation (1.44) and `helm template` customisation* — No values schema
  - ❌ SpacetimeDB: *Railway one-click template* — no PaaS templates (Helm/Terraform only)
  - 🟡 Agones/OM: *`install.yaml` single-file install with pre-generated webhook certs (not for prod); `helm template` to customise* — Dockerfile + Helm; no rendered one-file manifest published
  - 🟡 Beamable: *Terraform-codified infra, rebuild <1 h after regional outage; cross-region Mongo PIT backups* — Terraform modules shipped; backups/DR are operator's
  - 🟡 Colyseus: *Netcode stack (0.18+, all official SDKs): `defineInput()` schema, `room.input({type, mode reliable\|unreliable, history…* — match input channel (`MatchInput`/`MatchData`) with reliable/unreliable/sequenced channels, 1–60 Hz tick, server-side input buffer; no typed input schema shared with client, no client SDK side
  - 🟡 Metaplay: *AWS-only managed infra: EKS, Aurora MySQL, S3, CloudFront CDN for config archives, NLB; `metaplay get aws-credentials`…* — musubi: cloud-agnostic Terraform (generic K8s, Hetzner, OVH), no AWS/EKS profile, no CDN integration
  - 🟡 Metaplay: *SDK updates: `metaplay update sdk` (R36), compatibility matrix SDK↔Helm chart↔Unity↔.NET, release notes per version, de…* — semver crates + migration files; no compatibility matrix doc
  - 🟡 Nakama: *Database: any PostgreSQL wire-compatible DB; CockroachDB v20+ "officially supported", PostgreSQL 9.6+ "unofficially, de…* — Postgres 14+ only (README); no CockroachDB support (rg -i cockroach → none)
  - 🟡 Rivet: *Platform guides: Vercel, Cloudflare Workers, Supabase, AWS Lambda, Cloud Run, Kubernetes, Railway, Render, AWS ECS, Coo…* — Helm, Terraform (generic, Hetzner, OVH), Dockerfile (README L583); no PaaS one-click templates (`rg -i railway\|coolify` = 0)
  - 🟡 Rivet: *Examples/templates gallery (hello-world, chat-room, inspector-tabs, Railway starters, Vercel-flavored examples)* — `examples/` WASM modules + sim SDK; no starter templates per platform
  - 🟡 Rivet: *Docker Compose quickstart (`rivetdev/engine` + Postgres + NATS)* — `Dockerfile` + `docker run` Postgres in README quick start; no compose file (`rg -i compose` = 0)
  - 🟡 Rivet: *Render, AWS ECS, VM/systemd, Coolify guides* — Terraform modules for Hetzner/OVH/generic + Helm; no PaaS guides
  - 🟡 Rivet: *AMD64 + ARM64 Docker images* — Dockerfile builds locally; no published multi-arch image (`rg -i ghcr\|docker push` in CI = none)
- **Already covered rows (10):** Agones/OM: Terraform modules: GKE (Standard + Autopilot), EKS, AKS, OKE, helm3 s…; Colyseus: Scaling model: rooms pinned to one Node.js process, rooms spread even…; Edgegap: IaC; Metaplay: Cluster topology via Helm `shards` + runtime options: node sets with…; Metaplay: Deployment unit = `metaplay-gameserver` Helm chart with custom values…; Nakama: Docker Compose quick start (nakama + postgres/cockroachdb images, aut…; Rivet: Air-gapped / sovereign cloud deployment, Kubernetes manifests + Helm; Rivet: Kubernetes guide + Helm (Enterprise charts; community manifests); SpacetimeDB: Standalone single binary; SpacetimeDB: Linux self-host guide

#### P0-17. A/B experiments (variants, sticky assignment, phases, significance/scorecards, exclusion groups)  
*area:* progression/LiveOps · *musubi:* ❌ missing (1h/9p/11m over 23 rows)
- **Competitors with it (9):** Beamable, Colyseus, EOS, Hathora, Metaplay, Hiro/Satori, PlayFab, SpacetimeDB, UGS LiveOps
- **Why it matters:** Data-driven tuning; every major BaaS has it.
- **Implementation sketch (musubi terms):** Missing in core (Zanshin only). Add `experiments` (variants, % rollout, sticky hash assignment, phases, config overrides) in musubi-economy/liveops + outcome events to analytics; significance in Zanshin or SQL.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *A/B testing (Trials)* — README: A/B & experiments delegated to Zanshin (external); musubi only has audiences + flags, no trial/cohort allocation or data-override manifests
  - ❌ Metaplay: *Experiments management with targeting + private variant testing* — no A/B experiments in musubi (parity.md: via Zanshin)
  - ✅ Metaplay: *LiveOps Timeline (chronological view of events/offers/leagues/experiments/broadcasts schedules)* — `GET /v1/server/liveops/timeline?from&to&kinds&label`: live-event cycles (recurrence expanded, phases), flag rule windows, experiments, news, store offers, tournaments, scheduled economy changesets with `past|active|upcoming` + summary (by kind/state, active now, next 24 h); API only, no UI yet
  - ✅ Metaplay: *Experiment metadata (Analytics IDs) tagged on every analytics event; player keeps full experiment history* — every account-bound bus event carries `payload.experiments = {experiment_id: variant}` for the account's running experiments (stored, webhooks, analytics sinks, Zanshin); `GET /v1/server/experiments/of/{account_id}` lists current and past assignments
  - ❌ Hiro/Satori: *Experiments (A/B, multivariate up to 6 variants)* — rg -i "experiment\|variant\|a/b" in musubi -> only Zanshin passthrough mention; no experiment assignment or stats
  - ❌ PlayFab: *Experiments (A/B): control + up to 9 treatment variants, ≤10 variables per variant, Title Data Overrides as variants, t…* — No built-in A/B framework; README defers to Zanshin (external) for flags/experiments; grep experiment in crates: none
  - ❌ PlayFab: *Title Data Overrides: named override sets (GetTitleDataOverrides), applied per experiment variant / segment, ≤1 min pro…* — No override sets (Zanshin flags external)
  - ❌ UGS LiveOps: *A/B tests with statistical significance* — `rg -i experiment` only mentions Zanshin ("flags, experiments, analytics"); nothing in core computes assignment + significance. Sketch: keep delegation to Zanshin, add `experiment_variant` host op + exposure event
  - 🟡 Beamable: *Built-in stats (ADVERTISING_ID, CLIENT_VERSION, DATE_INSTALL, DATE_SESSION, DAYS_SINCE_INSTALL, PURCHASES_3D/7D/14D/28D…* — Account has created_at/last_login/platform; no auto-rolled purchase/session/spend window stats
  - 🟡 Colyseus: *`schema()` builder + `t.*` field builders (recommended) and classic `@type()` decorators (needs `experimentalDecorators…* — IDL (TOML/JSON Schema-like `musubi idl`) defines state types → `idl gen` bindings; `musubi idl from-colyseus` converts Colyseus Schema classes; no TS-first authoring
  - 🟡 Colyseus: *Streaming collections `t.stream(X)` / `.stream()` (experimental): drip additions `maxPerTick` (32) per client per encod…* — worlds: per-tick `budget_bytes` with priority by distance/recency (Interest v2); matches: no additive drip-feed
  - 🟡 EOS: *Title Storage: per-product encrypted read-only files uploaded via Developer Portal (same encryption key for all files),…* — musubi /v1/configs typed title config docs + content bundles + UGC blobs (S3); no CDN-style large file hosting for clients, no per-client/per-player overrides (audiences exist for stores/mail only), no portal upload UI
  - 🟡 Hathora: *GetLatestProcesses with filters* — `GET /v1/server/fleets/{id}/servers` lists per fleet; no cross-fleet filtered/paged process search or count endpoint
  - 🟡 Hiro/Satori: *Experiment recipes (store item variants, VIP tournaments, UI themes) and QA->5%->25% rollout phases* — musubi has canary rollout for WASM modules and fleet builds (percentage/cohort), not for config values
  - 🟡 PlayFab: *Player segments: filter engine (location, platform, login timespan, statistic value, VC balance, tags, push enabled, li…* — musubi: audiences (named rules, `/v1/admin/audiences`) used by store/mail/matchmaker; no continuous membership tracking, no enter/exit events, fewer filter dimensions
  - 🟡 PlayFab: *Segment usage: store segmentation (segment-exclusive stores / price overrides), Experiments targeting, Trends dimension…* — Audiences used by store listings & module canary pins; no campaign send-to-audience
  - 🟡 SpacetimeDB: *Row-level security (SQL filters)* — per-collection read permission (owner/public/server) in storage; no expression-based row filters
- **Already covered rows (1):** PlayFab: Title Data (public KVPs, 10KB values, cached ≤15 min), Internal Title…

#### P0-18. Progression tracks / XP / levels / skill trees / timed unlockables  
*area:* progression/LiveOps · *musubi:* 🟡 partial (12h/11p/1m over 24 rows)
- **Competitors with it (9):** AccelByte, brainCloud, EOS, Hathora, LootLocker, Hiro/Satori, Nakama, SpacetimeDB, UGS LiveOps
- **Why it matters:** Baseline.
- **Implementation sketch (musubi terms):** Exists: progression tracks, graphs.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hiro/Satori: *Personalize content recipes (discounted shop for veterans, DLC for spenders, gate feature by progression, welcome-back…* — no segment-targeted config variants in musubi
  - 🟡 AccelByte: *Challenge API (40): config (10), goals (7), schedules (4), plugins (Extend assignment), progression (5: evaluate, parti…* — quests + rewards + periods; no schedule/rotation objects or assignment plugins
  - 🟡 brainCloud: *Stats Events (macros that fire rewards)* — `quest_event` / progression XP via match results & WASM; no portal-defined stat macro
  - 🟡 LootLocker: *Progressions: tiered point-based tracks (threshold per tier, tier data), attachable to Player, Character Class, or Asse…* — Progression tracks with tiers + rewards per account (`/v1/progression/*`, graphs/skill trees, battle pass seasons); no progression attached to item instances or characters
  - 🟡 LootLocker: *Metadata entries on Progressions, Leaderboards, Catalog items, Currencies, Players: types String/Base64/Boolean/Number/…* — Free-form `metadata`/`attributes` JSON on items, leaderboards, records, store, accounts; storage collections with read/write permissions; no typed per-key access level or tag-based metadata query across systems
  - 🟡 LootLocker: *Unified Reward types selectable everywhere (Triggers, Leaderboards, Progressions, Catalogs, Campaigns, Twitch): Asset,…* — Reward tables grant items/currency/xp/entitlements/mail (`/v1/server/rewards/tables`); no platform key, Discord role, file, or metadata reward kinds
  - 🟡 LootLocker: *Game API misc: Submit XP (legacy), Universal assets list, Activate Rental Asset (legacy), Messages list (legacy), Missi…* — Quests/achievements/streaks replace missions; no trigger key or platform key list
  - 🟡 LootLocker: *Publisher tier: cross-game loyalty, publisher-wide currencies/progression (catalog items for publisher currency/progres…* — Multi-project in one cluster + account linking, but no cross-project wallets/progression or portfolio-level segments
  - 🟡 Hiro/Satori: *Progression dependency tree (saga map / skill tree)* — README "Progress graphs": `POST /v1/server/graphs` nodes with `requires`, rank costs, max_rank, reward per rank, unlock_seconds; missing cross-system preconditions (achievements/stats/items/energy thresholds) and cron r…
  - 🟡 Hiro/Satori: *Timed unlockables (chests) with slots* — musubi graph nodes with `unlock_seconds` + `skip_cost` (timed unlockables) but no slot system, queueing, probability-based random unlockable grant, or slot purchase
  - 🟡 Nakama: *Players: search by user ID/username/email/display name (`console.disable_display_name_search`), view/edit account, frie…* — API-only equivalents: `GET /v1/admin/accounts?…`, `/export`, `/restore`, `/ban`, `/kick`, `/sessions`, `/limits`, `/anticheat`, `/reports`; no UI, no tombstone table (rg -i tombstone → none)
  - 🟡 Nakama: *Runtime context fields: env, execution mode, node, version, headers, query params, user id/username/vars, user session…* — module calls receive account/session/project context; no match-tick context for non-match code
- **Already covered rows (12):** AccelByte: Season Pass: seasons (draft/publish, localization, start/end, default…; AccelByte: Season Pass API (45): seasons (23: CRUD, publish/unpublish, clone, ex…; brainCloud: XP & levels; brainCloud: Rewards (XP, currency, stats, achievements) + `RegisterRewardCallback`; EOS: Progression Snapshot Interface: per-user key/value progression snapsh…; Hathora: Progression, achievements (platform sync), quests, seasons, rewards,…; LootLocker: Scheduled reset via cron / Quick Cron presets, rank-range Rewards (as…; LootLocker: Legacy XP & level per player + `get-multiple-other-players-xp-and-lev…; Nakama: Data modeling guidance: event-driven progression templates instead of…; Nakama: Hiro metagame framework (economy, inventory, progression, energy, ach…; SpacetimeDB: Progression / quests / seasons / achievements; UGS LiveOps: Webhook actions

#### P0-19. Segments / audiences (rule builder, tracked membership, enter/exit actions, scheduled refresh, composable algebra)  
*area:* progression/LiveOps · *musubi:* 🟡 partial (2h/26p/7m over 35 rows)
- **Competitors with it (9):** Beamable, brainCloud, Colyseus, LootLocker, Metaplay, Hiro/Satori, PlayFab, SpacetimeDB, UGS LiveOps
- **Why it matters:** Targeting backbone for offers/pushes/events.
- **Implementation sketch (musubi terms):** Exists: static + rule-based audiences (`/v1/server/audiences*`). Gap: any/all algebra, computed props (account age, hash bucket, country), tracked membership with enter/exit actions, per-player segment view.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *Dashboard widgets: presets `totals`, `recentUsers`, `liveRooms` (3 s refresh), `health` (DB latency), `segments`; custo…* — Prometheus `/metrics` + Grafana is the only dashboard path
  - ❌ LootLocker: *Squire Discord bot: install to server, link to game; Claimable Rewards = Trigger + Squire tab (title/body markdown, ima…* — rg -i discord → OIDC login only; no Discord bot / community-distribution surface
  - ❌ Metaplay: *Per-entity event log persisted in segments (`EventLogForPlayer/Guild` options: RetentionDuration, NumEntriesPerPersiste…* — no per-player event history store
  - ✅ Hiro/Satori: *Includes/Excludes identity overrides; managed (imported, read-only) audiences from external analytics; default audience…* — `PUT /v1/server/audiences/{name}/overrides {include, exclude, note, expires_in_secs}` (include beats rules/imports, exclude beats everything), `kind: managed` audiences imported via `PUT …/members {account_ids, mode: replace|add|remove}` / `POST …/members/csv`, `source` tag; a `rules: {}` audience is the default/everyone audience
  - ❌ Hiro/Satori: *RoAS report: CPI from MMP (Adjust) × LTV (IAP + ad revenue) per cohort; filters by channel/campaign/country/variant* — none
  - ❌ PlayFab: *Player Churn Prediction (paid premium add-on): ML churn-risk scoring for players active in last 21 days, 48h lead time,…* — No ML/predictive segments
  - ❌ PlayFab: *Trends via API: Segments-as-dimension configuration via API/Game Manager, ≤10 segments)* — None
  - 🟡 Beamable: *Game Base Cloud Data (per-game blobs with cohort overrides)* — Content bundles / storage global collections; no cohort override manifest
  - 🟡 brainCloud: *DataStream (analytics export)* — event bus → Zanshin LiveOps (`[zanshin]`), webhooks with retry; no Segment/Kafka sink, no crash reports, no client custom-event API
  - 🟡 brainCloud: *Segments* — named audiences `[audiences]` by account fields (README); no self-serve rule builder, no spend/behaviour predicates
  - 🟡 LootLocker: *Broadcasts (in-game news/MOTD): org-wide targeting multiple games, multiple publication schedules with start/end + time…* — musubi has in-game mail with audience targeting and schedules (`/v1/server/mail`, Zanshin live events) and `/v1/groups/{id}/announcement`; no localized multi-language news objects, no per-player-timezone scheduling, no…
  - 🟡 LootLocker: *Scheduled Reward Campaigns: scheduled_at, max rewards cap, sort None/Random (lottery), segment targeting (OR), statuses…* — Mail with audience + reward attachments and schedules can grant to an audience at a time; no winner cap/random lottery pick, no winners report
  - 🟡 LootLocker: *Triggers: key, Unlimited/Limited per-player invocation limit, segment gating, Reward or Reward Group; invoke multiple k…* — Achievements/quests with grant limits + reward tables and WASM `rpc` hooks cover server-defined rewards; no generic client-invokable "trigger key" primitive with per-player limit & segment gate (rg trigger = 1, event bu…
  - 🟡 LootLocker: *Player Segments (AND conditions, multiple segments = OR): Are Playing On platforms, Have Linked platform accounts, Have…* — Named audiences `/v1/server/audiences` (lang, location, platform, age_bracket, created window, progression track level, active/inactive days, metadata) with count; no cross-game membership, entitlement/item-ownership, l…
  - 🟡 LootLocker: *Limit Access (Oct 2026): only players matching ≥1 Segment may start a session; enabling clears all sessions; error to d…* — Allow-list via bans/age-gate/login queue; no segment-based session gate
  - 🟡 Metaplay: *Player segments page: browse segments, see usage by LiveOps features and player counts* — `/v1/server/audiences` compute membership; no usage cross-reference or size estimates UI
  - 🟡 Metaplay: *Player segments defined in game config sheet: `PropId/PropMin/PropMax` parallel arrays (AND), `RequireAnySegment` (unio…* — musubi `audiences` (migration `audiences`, `/v1/server/audiences`) filter on account/storage fields; no composable segment algebra (any/all), no hash-bucket subsets, no fixed-point/time-since-login properties documented
  - 🟡 Metaplay: *Segment usage in custom code via `PlayerCondition.MatchesPlayer` on client and server; segment page shows player counts…* — audience membership evaluation server-side only; no per-player segment list endpoint
  - 🟡 Hiro/Satori: *Audience filter expressions (expr language)* — musubi named audiences (`PUT /v1/server/audiences/{name}` rules: lang, location, platform, age_bracket, created_after/before, track+min/max_level, active_within_days, inactive_days, metadata; returns count) — fixed rule…
  - 🟡 Hiro/Satori: *Recompute: bulk every 10 min (`audience.processor_interval_sec`), per-player recompute on every event (ComputedProps +…* — musubi audiences are evaluated at send time (mail broadcast / count); no stored membership or event-triggered recompute
  - 🟡 Hiro/Satori: *Player messaging: in-game inbox + push/email* — musubi: mail (`/v1/mail`, broadcasts by audience, rewards attached, TTL, read state) + notifications + push templates (FCM/APNs/HMS/WebPush, schedule, tz) + SMTP mailer for auth mails; no HTML/JSON templating with prope…
  - 🟡 PlayFab: *Segments: filters (location, platform, login timespans, stats, VC, tags, push enabled, linked providers, LTV, event cou…* — musubi audiences (rule DSL) and `/v1/admin/accounts/export`; no export-by-audience, no actions, no tracked membership
  - 🟡 PlayFab: *Scheduled Tasks: run CloudScript or segment actions on cron-like schedule (one-time/recurring, UTC), per-player tasks o…* — README module schedules (cron) + timers run server code; no fan-out-over-segment task primitive with per-task history
  - 🟡 PlayFab: *Scheduled Tasks (Automation): run CloudScript at cron-like schedule or run actions on every player in a segment (10–15…* — README module schedules/cron + timers; no segment fan-out task runner
  - 🟡 PlayFab: *Scheduled task types: CloudScript task (function + args), ActionsOnPlayersInSegment task (actions list, 10–15 tasks/sec…* — README module schedules (cron) + timers; no segment-fan-out tasks or instance history API
  - 🟡 PlayFab: *Segment filters catalog: FirstLogin / LastLogin (date/timespan), Location (country/continent), Platform (OS/device/acco…* — README audiences DSL over account fields/metadata/labels/wallet/stats; no LTV/location/platform/push-enabled fields, no membership tracking
  - 🟡 PlayFab: *Segment APIs: GetAllSegments, GetPlayerSegments (client/server), GetPlayersInSegment (continuation, SecondsToLive ≤ 3 h…* — `/v1/admin/audiences` evaluate/list; no "players in audience" pagination export, no membership events
  - 🟡 PlayFab: *Account/engagement events: player_logged_in (platform, device, location), player_added_title, player_created, player_li…* — README account/auth events (registered, login, linked, banned, erased, email verified, push registered) + notification sent; no ad, session/focus, device-info, cloudscript-executed events
  - 🟡 SpacetimeDB: *In-memory state + commitlog durability* — musubi keeps world/match journals + snapshots (`0074 world journals`, `0075 match journals`, `0070 world snapshots`) for sims, but durable data of record is Postgres (WAL), not an in-process commitlog
  - 🟡 UGS LiveOps: *Stateful audiences (Analytics audience membership used in overrides)* — `GET /v1/server/audiences/of/{account_id}` + `audiences.of` host op; audiences are rule-evaluated, not behavioural (no analytics-derived cohorts)
  - 🟡 UGS LiveOps: *Overrides across content types (Remote Config settings, Economy config, Cloud Code?) with targeting + scheduling + repo…* — Zanshin flags/live-events integration + audiences + listing windows; no unified override object, no KPI reporting
  - 🟡 UGS LiveOps: *Audiences (standard + custom, behavioural)* — Rule-based named audiences exist (`/v1/server/audiences`), with activity windows but not event-behaviour cohorts
  - 🟡 UGS LiveOps: *Campaign composer in dashboard (send once / recurring, audience, environment)* — Server API `POST /v1/server/push` with `send_at`, local-time delivery, templates, audiences via `account_ids`/everyone; no recurring campaigns, no UI
- **Already covered rows (2):** PlayFab: Player Custom Properties (public preview): ≤5 props per player, 36-ch…; PlayFab: CloudScript using Azure Functions: ExecuteFunction (HTTP/queue/durabl…

#### P0-20. Native C/C++ client SDK (consoles, custom engines)  
*area:* SDKs · *musubi:* ❌ missing (2h/4p/8m over 15 rows)
- **Competitors with it (8):** Agones/OM, Colyseus, EOS, Meshing vendors, Metaplay, Photon, PlayFab, SpacetimeDB
- **Why it matters:** Console builds and custom engines need a C ABI.
- **Implementation sketch (musubi terms):** Build `musubi-client-core` in Rust with C ABI (`cbindgen`) wrapping REST + realtime; consoles via platform TLS hooks.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *Client callbacks: `getStateCallbacks(room)` → `listen(prop)`, `bindTo(obj, props)`, `onChange` (direct props only), `on…* — no client SDKs: integrators apply `match_state_delta` JSON Patches themselves; `musubi idl gen` emits types but no change-callback runtime
  - ❌ EOS: *EOS Platform Interface: single C ABI SDK handle to all interfaces; multiple instances; application status (suspend/resu…* — no client SDK (only Unreal scaffold)
  - ❌ Photon: *Fusion Core 3 (C++20, on request)* — no C/C++ client core
  - ❌ PlayFab: *Game Saves (preview, 2025): cross-device save-file sync via GDK/Unified SDK, 256MB per player per title, 100 requests/2…* — No save-file sync SDK (storage is JSON KV; no file sync/conflict resolution client component)
  - ❌ PlayFab: *Unified C SDK event pipeline: batched telemetry/PlayStream upload with retry, offline buffering, compression; debug tra…* — No client telemetry pipeline
  - ❌ PlayFab: *Unified C SDK design: PFServices init, async XAsync model (XAsyncBlock, queues), memory hooks (PFMemSetFunctions, memor…* — No client SDK
  - 🟡 Colyseus: *Schema codegen `schema-codegen` for C#, C++, Haxe, Dart targets from server schema definitions* — `musubi idl gen` generates state types from IDL for Rust/TS/C#; no C++/Haxe/Dart/GDScript, no decoder runtime
  - 🟡 EOS: *Docs structure: get-started guides (C++/C#), per-interface guide+reference, how-to guides, samples per feature, SDK err…* — musubi README (one row per endpoint) + docs/parity.md + OpenAPI; no narrative guides/samples/error catalogue
  - 🟡 Meshing vendors: *Worker SDKs* — Rust sim SDK + raw HTTP/WS; no C/C#/Java client libs
  - 🟡 Metaplay: *WebGL support: WebSocket transport, browser-native client preview (R38), IL2CPP notes* — WS/WebTransport/WebRTC server transports exist; no browser SDK
- **Already covered rows (2):** Colyseus: Versioning: append-only fields, `.deprecated()` keeps slot, backwards…; Meshing vendors: Language

#### P0-21. TypeScript / JavaScript / Web SDK (+ React hooks, Node server SDK)  
*area:* SDKs · *musubi:* ❌ missing (4h/3p/6m over 18 rows)
- **Competitors with it (8):** Agones/OM, Beamable, Colyseus, GameLift, Hathora, Metaplay, Rivet, SpacetimeDB
- **Why it matters:** Web games, tools, and bots.
- **Implementation sketch (musubi terms):** Generate TS client from `openapi.json` (openapi-typescript) + realtime client; publish `@musubi/client` and `@musubi/server`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Web SDK (`@beamable/sdk`, TypeScript, MIT)* — No JS/TS SDK; OpenAPI only
  - ❌ Hathora: *TypeScript SDK* — README: client SDKs not written; `GET /openapi.json` for generation
  - ❌ Metaplay: *LiveOps Dashboard SPA (Vue 3 + TypeScript, source-available)* — README: "client SDKs and the admin console are not written yet"; musubi exposes only `/v1/admin/*` HTTP routes
  - ❌ Rivet: *JavaScript/TypeScript client `@rivetkit/client` (Node, Bun, Deno, browser), `createClient(endpoint)`, typed from regist…* — no JS/TS client (README L8: "client SDKs not written yet"; `rg -i typescript` only in IDL/deploy)
  - ❌ SpacetimeDB: *TypeScript / JavaScript SDK* — none
  - 🟡 Colyseus: *Node.js/TypeScript server framework (`defineServer`, `npm create colyseus-app`), Bun runtime supported* — musubi server logic = sandboxed WASM modules (`musubi-runtime`, match_<module>_* exports) — no JS/TS runtime; WASM gives isolation (fuel/memory/deadline) Colyseus lacks
  - 🟡 Rivet: *TypeScript helper types (`ActionContextOf`, `CreateContextOf`…), end-to-end typed client from `typeof registry`* — `musubi idl` emits bindings (Rust complete, README L369) and schema-derived types; no TS/C# client type generation yet
  - 🟡 Rivet: *Generated engine API SDKs: Go, Rust, TypeScript (`engine/sdks`)* — `GET /openapi.json` lets users generate; nothing pre-generated
- **Already covered rows (4):** Beamable: Web SDK `BeamWebSocket` session-start frame & websocket notifications; Colyseus: Transport: WebTransport / HTTP3 (`@colyseus/h3-transport`, experiment…; Colyseus: WebTransport (HTTP/3, experimental): `app`, `cert`, `key`, `server`,…; Colyseus: Collections: `ArraySchema` (push/pop/shift/unshift/splice/shuffle/mov…

#### P0-22. Product analytics dashboards (DAU/MAU, retention, funnels, revenue, sessions)  
*area:* analytics/observability · *musubi:* ❌ missing (1h/8p/13m over 25 rows)
- **Competitors with it (8):** AccelByte, Beamable, brainCloud, LootLocker, Metaplay, Hiro/Satori, PlayFab, UGS LiveOps
- **Why it matters:** Studios expect basic KPIs out of the box.
- **Implementation sketch (musubi terms):** Add SQL views + shipped Grafana "LiveOps KPI" dashboard over `analytics_events`/`ledger`/`sessions`; Zanshin for deep analytics.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Portal per-player analytics view & download* — No UI
  - ❌ Beamable: *Portal Analytics KPI page & API Usage page* — No UI; metrics via Prometheus only
  - ❌ Metaplay: *Business metrics dashboard (DAU, revenue, retention cohorts; custom StatisticsEventType time series, bucket accumulatio…* — Prometheus technical metrics only; no business KPI aggregation
  - ❌ Hiro/Satori: *Analytics dashboards: built-in Status + key metrics (DAU, retention, ARPDAU, revenue), custom shared dashboards from wi…* — musubi exposes Prometheus/OTel operational metrics only; no product analytics dashboards
  - ❌ Hiro/Satori: *Retention report D1–D30 cohorts, cumulative mode, lifetime engagement (session count/duration/playtime), filter by coun…* — no retention analytics (retention.rs = data retention/erasure policy)
  - ❌ Hiro/Satori: *Funnels: unlimited / time-based full / step conversion / session-based; filters on value, metadata, synthetic events; n…* — none
  - ❌ PlayFab: *Data Explorer (public preview): basic mode (top 100 events, condition builder) and advanced KQL over events.all table;…* — No built-in analytics query engine (Postgres only, no event warehouse)
  - ❌ PlayFab: *Reports: Daily/Monthly/Rolling-30 Overview & Totals, 30-day Retention, 30-day New User Retention, 30-day New User Conve…* — No reporting layer; metrics only via Prometheus counters + external Zanshin
  - ❌ PlayFab: *Trends: DAU, MAU rolling-30, avg duration/on-screen intervals (client_focus_change), Nth-day new-user retention, revenu…* — No DAU/retention/revenue dashboards
  - ❌ PlayFab: *Daily title report email (DAU, new, MAU, logins, D1/D7 retention), opt in/out per studio member* — None
  - ❌ UGS LiveOps: *Dashboards (standard + custom)* — No analytics UI; Prometheus/Grafana for ops metrics only. Sketch: ship Grafana dashboards over ClickHouse/Postgres event tables or rely on Zanshin console
  - ❌ UGS LiveOps: *Data Explorer v1 / v2 (DAU, WAU, MAU, D1/D7/D30 retention, ARPDAU, aggregate filters)* — No product-analytics metrics computed in musubi (`rg -i funnel / mau` → 0); delegated to Zanshin
  - ❌ UGS LiveOps: *Funnels* — Delegated to Zanshin
  - 🟡 AccelByte: *Analytics dashboards (free): DAU/WAU/MAU/HAU, peak, new vs total registered, retention, engagement, matchmaking; dimens…* — metrics + audiences/segments; no built-in DAU/MAU/retention dashboards (README "Analytics": events export only)
  - 🟡 AccelByte: *AIS (AccelByte Intelligence Service, existing customers only): data warehouse (read-only PostgreSQL 15 endpoint, summar…* — events/telemetry queryable in own Postgres; no warehouse connectors (S3/Redshift/Snowflake)
  - 🟡 Beamable: *Built-in platform telemetry KPIs* — Audit + domain events emitted (economy, social) but no derived session/spend enrichments
  - 🟡 brainCloud: *Analytics dashboards* — no built-in analytics; events to Zanshin/Prometheus (`musubi_*` metrics)
  - 🟡 brainCloud: *App Alerts (health)* — Prometheus/OTel export lets operators alert externally; no built-in health alert rules or Slack sink (grep `slack` config only)
  - 🟡 LootLocker: *Insights: DAU history, New Player history, Total Players per Platform, Presence average playtime & locations, Progressi…* — Prometheus metrics (`/v1/metrics`: CCU, auth, matches), per-player stats (`/v1/server/stats/*`) and matchmaker stats; no DAU/MAU/retention product analytics or export
  - 🟡 PlayFab: *Data Connections (export): Azure Blob Storage (SAS, West US 2, ≤3 enabled), Azure Data Explorer table ingestion, Micros…* — musubi: webhooks (HMAC, batching?) + `musubi export` JSONL; no managed sink connectors (blob/ADX/Fabric)
  - 🟡 UGS LiveOps: *SQL Data Explorer* — You own Postgres: raw SQL over `events`/`audit` tables possible, no hosted explorer or curated fact tables
- **Already covered rows (1):** LootLocker: Rate limits: not documented publicly; MAU-based plan limits (10k MAU…

#### P0-23. Parties (leader, invites, join requests, party data, matchmaking as unit)  
*area:* social · *musubi:* 🟡 partial (9h/6p/5m over 22 rows)
- **Competitors with it (8):** AccelByte, Beamable, brainCloud, Nakama, Photon, PlayFab, Pragma, UGS Multiplayer
- **Why it matters:** Baseline for team play.
- **Implementation sketch (musubi terms):** Exists: WS `party_*`. Gap: party label/hidden flag + listing/search (Nakama 3.28).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Unreal party/team config in PIE presets (2.3)* — No engine tooling
  - ❌ PlayFab: *Party SDKs: C/C++ (Android, iOS, Linux, macOS, Win10/8.1, Switch, PS4, PS5, PC GDK, Xbox GDK), Unity plugin (public Git…* — No client-side networking SDK/plugins in musubi
  - ❌ PlayFab: *Party API objects: PartyManager (Initialize, CreateNewNetwork, ConnectToNetwork, GetRegions, StartProcessingStateChange…* — No equivalent client networking object model; musubi realtime protocol is server-relayed messages/channels
  - ❌ PlayFab: *Party Xbox Live Helper Library (PartyXboxLive): XSTS-based cross-network permission checks (communication privileges, c…* — No Xbox privacy/privilege enforcement helpers
  - ❌ PlayFab: *Party Unity plugin (PlayFabMultiplayerManager: CreateAndJoinNetwork/JoinNetwork/LeaveNetwork, SendDataMessage, text cha…* — None
  - 🟡 brainCloud: *Third-party hosting integrations* — GameLift, Edgegap, Hetzner/OVH Terraform, SSH bare-metal (README); no i3D.net
  - 🟡 Photon: *Quantum Menu sample* — lobby codes exist server-side; no UI
  - 🟡 PlayFab: *PlayFab Party: cross-platform low-latency networking + chat; networks 2–128 devices, cloud relay in Azure regions (auto…* — musubi: authoritative server relay over WS/QUIC/WebTransport/WebRTC-DC/UDP with channels (reliable/unreliable) — no managed P2P mesh/relay product, no client networking SDK; voice via Aurix tokens
  - 🟡 PlayFab: *Party accessible chat: voice chat w/ chat controls, permissions (mute/block per target), text chat, speech-to-text, tex…* — musubi chat: text, moderation hooks (external), translation via `chat.translate_url` webhook (README) — external service, voice = Aurix integration only, no STT/TTS
  - 🟡 Pragma: *Party custom data (public/private/player ext)* — party `metadata` JSON + per-member metadata; no typed schema/visibility split (public vs private) enforced server-side.
  - 🟡 Pragma: *Game-server version compatibility gating* — tickets carry `build`/`version` attributes matched by rules; no first-class compatibility matrix config.
- **Already covered rows (9):** AccelByte: Party (Session V2 party sessions): create (one per player), invite/ca…; Beamable: Parties; Beamable: Party: invite, promote leader, max size, restriction, `PartyMember` o…; Nakama: Parties are per-node (not clustered in OSS); Enterprise clusters rout…; Pragma: Party service; Pragma: Party preferred game-server zones & ping; Pragma: Stale party expiration; Pragma: Party invite expiry & single-party rule; UGS Multiplayer: Party as pre-matchmaking group

#### P0-24. Godot SDK / GDScript addon  
*area:* SDKs · *musubi:* ❌ missing (1h/0p/7m over 8 rows)
- **Competitors with it (7):** Agones/OM, Colyseus, Edgegap, Hathora, LootLocker, Photon, SpacetimeDB
- **Why it matters:** Fast-growing indie engine.
- **Implementation sketch (musubi terms):** Generate GDScript/GDExtension client from openapi + realtime client; publish to Asset Library.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *Third-party SDKs: Kotlin (Cubxity, scrayosnet), Godot, Java (agones4j), Scala (zio-agones)* — No third-party musubi SDKs
  - ❌ LootLocker: *Godot SDK (4.3+, Asset Library / GitHub, LootLockerSettings.cfg) — subset of features* — none
  - ❌ Photon: *Fusion Godot 3 (preview)* — rg godot=1 (mention only)
- **Already covered rows (1):** Colyseus: v1.0 roadmap: stabilize netcode API, close SDK gap (Godot struct offs…

#### P0-25. SDK ergonomics: auto-reconnect/backoff, request bundling, compression, RTT/jitter sliders, error reports  
*area:* SDKs · *musubi:* 🟡 partial (4h/6p/2m over 12 rows)
- **Competitors with it (7):** AccelByte, brainCloud, Colyseus, GameLift, Metaplay, Pragma, Rivet
- **Why it matters:** Robust clients.
- **Implementation sketch (musubi terms):** Design into generated clients (shared `musubi-client-core`).
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *SDK features: config injection, multi-environment settings, multi-registry, visual debugger, SDK log config, WebSocket…* — no SDK → no SDK-level tooling
  - ❌ brainCloud: *Client configuration surface* — no client library; server supports lz4 compression on realtime, HTTP batching absent (grep `batch` = bulk admin ops only)
  - 🟡 brainCloud: *Auto-reconnect / long session* — server side: refresh-token rotation; client-side reconnect helpers only in Unreal scaffold `sdk/unreal`
  - 🟡 Colyseus: *Schema-based authoritative state (`@colyseus/schema`): server mutates, property-level change tracking (ChangeTree, refI…* — typed match state via IDL `[matches.<module>] state`: snapshot `match_state` on join + per-tick `match_state_delta` as RFC 6902 JSON Patch (json or postcard); no custom binary field-level codec, no handshake of types (c…
  - 🟡 Colyseus: *Reconnection options: `enabled` true, `maxRetries` 15, `minDelay` 100 ms, `maxDelay` 5000 ms, `minUptime` 5000 ms, `max…* — server-side resume tokens + grace periods exist; retry/backoff/queue must be implemented by each studio client
  - 🟡 Metaplay: *`MetaplayClient` session lifecycle: `ConnectAsync`, `IMetaplayLifecycleDelegate`, `ConnectionLostEvent` with ~40 `Techn…* — server side has session resume tokens + single-session kick (README session row); Unreal SDK is "v0 scaffold" without auto-reconnect; no Unity SDK
  - 🟡 Rivet: *Crash policy: `crash_policy: restart\|sleep\|destroy`, Crash Loop Backoff status* — match failover re-hosts once from snapshot (`musubi-actor` failover, `backoff` in cluster/mesh); no configurable crash policy per actor type
  - 🟡 Rivet: *Actor statuses: Starting, Running, Destroyed, Crashed, Sleeping, Pending, Crash Loop Backoff + timestamps (createTs, co…* — match/world/gameserver status enums (`GET /v1/server/matches/{id}`, `GET /v1/server/gameservers/{id}`: pending/allocated/running/draining/ended) — fewer states, no crash-loop detection
- **Already covered rows (4):** Colyseus: Reconnection: `allowReconnection(client, seconds \| "manual")` seat h…; GameLift: InitSDK / ProcessReady / ProcessEnding; Pragma: Party tasks: CreatePartyV1 / JoinPartyWithInviteCodeV1 / JoinPartyWit…; Rivet: Step retries: `maxRetries`, exponential `retryBackoffBase/Max`, `time…

#### P0-26. Client telemetry / event ingestion (batched events, schema registry, pre-login keys, sampling)  
*area:* analytics/observability · *musubi:* 🟡 partial (3h/6p/5m over 14 rows)
- **Competitors with it (7):** AccelByte, Beamable, Metaplay, Hiro/Satori, PlayFab, Pragma, UGS LiveOps
- **Why it matters:** Without events there is no analytics.
- **Implementation sketch (musubi terms):** Missing. Add `POST /v1/telemetry/events` (batched, schema-validated, pre-login telemetry key) → `analytics_events` partitioned table + sinks (musubi-integrations).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Metaplay: *Client-side analytics event delegate (forward to Firebase etc.)* — no client SDK
  - ❌ Hiro/Satori: *Client analytics system (Unity) with providers Satori, Firebase, GameAnalytics, Smartlook; automatic session events* — no client SDK / analytics; `POST /v1/analytics`? rg "analytics" -> only parity doc mention of Zanshin
  - ❌ Hiro/Satori: *Event taxonomy: core events (~65 predefined incl Hiro system events, adImpression/purchaseCompleted with Event-Revenue-…* — musubi has no analytics event ingestion from clients (events bus is server-emitted domain events only); Zanshin integration forwards server events
  - ❌ PlayFab: *Telemetry events: WriteTelemetryEvents (10KB payload, softer limits), Telemetry Keys (≤5 per title, no entity token, ex…* — No unauthenticated telemetry ingestion keys (grep telemetry: none)
  - ❌ PlayFab: *WriteEvents (entity events, custom namespace, ≤10KB body, EntityEventsPer15Seconds 25 tier), WritePlayerEvent / WriteTi…* — No client-facing event write endpoint (grep `/v1/events`: none; server-side event bus only)
  - 🟡 Beamable: *Analytics events pipeline* — Event bus → webhooks/Zanshin sink (README analytics row); no hosted warehouse, no SQL access, no out-of-box MAU/DAU/retention queries
  - 🟡 Metaplay: *Typed analytics events (`[AnalyticsEvent(typeCode, displayName, docString, includeInEventLog, sendToAnalytics)]`), emit…* — `/v1/server/events` event bus accepts custom events from WASM/game servers; no typed schema registry or per-event docs
  - 🟡 Hiro/Satori: *Built-in server analytics events (~60 names)* — musubi emits domain events (account.*, wallet.*, inventory.*, achievement.unlocked, purchase, match, leaderboard ...) on the bus/webhooks but no analytics pipeline/metrics (value/metadata shape) and no purchase USD norm…
  - 🟡 PlayFab: *PlayStream events: PlayFab-generated standard-schema events (com.playfab / events namespace: player_logged_in, entity_c…* — musubi event bus emits typed server events to webhooks (migration 0027 events); client-emitted custom analytics events endpoint: README `POST /v1/events` (check) — no standard event catalog comparable to PlayStream
  - 🟡 Pragma: *Telemetry event ingestion* — musubi has events bus + webhooks + Zanshin LiveOps analytics integration (`musubi-integrations`); no generic client telemetry ingestion endpoint for arbitrary game events (`rg -i telemetry` → OTLP tracing only).
  - 🟡 UGS LiveOps: *Custom events + Event Manager* — `POST /v1/server/events` accepts arbitrary server events (route in `ROUTES`) → webhooks/Zanshin; no schema registry/Event Manager
- **Already covered rows (3):** AccelByte: Game Telemetry: client/server event ingestion (batch interval, flush)…; Beamable: Analytics event batching (max 100, time/size flush); Pragma: Pragma analytics events

#### P0-27. MFA / 2FA (TOTP, backup codes, email code) incl. admin enforcement  
*area:* auth · *musubi:* 🟡 partial (1h/7p/1m over 12 rows)
- **Competitors with it (7):** AccelByte, Beamable, brainCloud, LootLocker, PlayFab, SpacetimeDB, Steamworks
- **Why it matters:** Account takeover protection; admins often required to use it.
- **Implementation sketch (musubi terms):** Exists: TOTP + recovery codes (`/v1/account/mfa/totp*`). Gap: email-code 2FA, enforced MFA for operator keys/SSO.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Steamworks: *Family Sharing awareness* — no notion of borrowed licenses; entitlements are flat per account
  - 🟡 AccelByte: *2FA for players and admins: authenticator app (TOTP), 10 backup codes, email code; critical event history of 2FA activa…* — TOTP + recovery codes (`/v1/auth/mfa/*`); no email-code 2FA; no admin-side 2FA enforcement for operators
  - 🟡 AccelByte: *Critical event history: admin-queryable log of sensitive account events (2FA on/off, etc.)* — admin audit log + `suspicious_login` event exist; no per-user security-event timeline endpoint
  - 🟡 AccelByte: *Admin SSO & 2FA for operators; admin roles per namespace; invite admins by email* — operator OIDC SSO + roles; no operator 2FA enforcement / email invites
  - 🟡 brainCloud: *Portal SSO (SAML) & team MFA* — operator SSO roles supported in config (`sso` README); no console
  - 🟡 LootLocker: *User settings: profile, password, passkey login to console (Aug 2026), 2FA* — Operators: SSO (OIDC) + TOTP for players; console does not exist
  - 🟡 PlayFab: *Game Manager developer accounts: Microsoft Entra / MSA auth required since 2024-07-27, 2FA, studio/title hierarchy* — musubi: operator SSO via OIDC (`[operators.sso]`) + server keys with roles; no studio/title hierarchy UI, no built-in 2FA for operators (TOTP exists for players only)
  - 🟡 PlayFab: *Studio settings: team members & roles (title/studio scope, custom roles over 80 permissions), SSO via Microsoft Entra,…* — Operator OIDC SSO + server-key roles (coarse: admin/server/readonly); no fine-grained permission matrix
- **Already covered rows (1):** SpacetimeDB: MFA / TOTP

#### P0-28. RBAC for operators: custom roles composed of permissions, console roles/ACL templates  
*area:* auth · *musubi:* 🟡 partial (1h/8p/0m over 9 rows)
- **Competitors with it (7):** AccelByte, Beamable, Metaplay, PlayFab, Pragma, Rivet, UGS LiveOps
- **Why it matters:** Larger studios need least-privilege for support vs liveops vs engineering.
- **Implementation sketch (musubi terms):** Exists: 5 fixed key roles. Add `operator_roles` with permission bitmask per route family (musubi-edge auth layer) and assignable to SSO users/keys.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *Permissions model `[ADMIN:][NAMESPACE:ns:][USER:uid:]OBJECT [CRUD]` per endpoint; roles = permission sets; default role…* — operator roles server/admin/support/liveops/ops + scoped server keys (README "Operator roles"); no per-endpoint CRUD permission strings, no custom role editor, no per-user-resource scoping
  - 🟡 AccelByte: *Roles API (48 ops): role CRUD, permissions per role, members, managers, admin role vs user role, override default role…* — fixed operator roles (server/admin/support/liveops/ops); no role editor
  - 🟡 Metaplay: *Roles & permissions: 4 predefined roles (Game Admin, Game Viewer, CS Senior, CS Agent); custom roles in `Options.base.y…* — musubi has 5 fixed operator roles (server/admin/support/liveops/ops) + scoped keys; no custom role definition with per-endpoint permission lists
  - 🟡 Metaplay: *Admin HTTP API with per-endpoint permission attributes, `GameAdminApiController` base, strongly-typed responses* — `/v1/admin/*` routes with operator roles; custom admin endpoints only via WASM RPC, no custom controller
  - 🟡 PlayFab: *Game Manager roles & permissions: >80 individual permissions, read/read-write each, built-in Title Admin / Dashboard ro…* — musubi: 5 fixed server-key roles (server/admin/support/liveops/ops) — no fine-grained per-feature permission matrix
  - 🟡 Pragma: *Portal RBAC* — API roles only.
  - 🟡 Rivet: *Enterprise RBAC/ACL: rule→policy→role→token, additive, resources actor/actor_gateway/actor_kv/runner/runner_config/name…* — operator roles + console roles + SSO for operators (README L321, `rg -i sso`), key revocation; no policy language / per-resource rules — but free vs Rivet's paid gate
  - 🟡 UGS LiveOps: *Org user RBAC (project roles per service), SCIM provisioning* — Operator SSO via OIDC (`/v1/admin/sso`) with role mapping; no SCIM
- **Already covered rows (1):** Beamable: Operator roles: tester / developer / admin / REPORT

#### P0-29. Observability UI (logs, metrics, usage, API usage page, billing/usage views)  
*area:* console/admin · *musubi:* ❌ missing (1h/3p/3m over 8 rows)
- **Competitors with it (7):** Beamable, brainCloud, Hathora, Meshing vendors, Metaplay, Nakama, UGS Multiplayer
- **Why it matters:** Ops visibility.
- **Implementation sketch (musubi terms):** Embed Grafana panels or expose Prometheus-backed charts in console.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Portal features from changelog 1.5–1.7 (2025-10 → 2026-06)* — No console
  - ❌ Metaplay: *NFT collection/NFT pages, league/season/division pages, matchmaker page, segments page, experiments page, offers page,…* — admin console "not written yet" (README status)
  - 🟡 brainCloud: *API Usage & billing reports* — per-key/per-account rate limits + Prometheus counters; no cost-oriented usage report
  - 🟡 Hathora: *Process metrics graphs in Console* — node/match/transport metrics only; per-dedicated-server resource series missing
  - 🟡 Meshing vendors: *Metrics viewer* — Prometheus `/metrics` but no bundled dashboards (`rg grafana` 0)
- **Already covered rows (1):** UGS Multiplayer: Relay REST API usage without SDK (engine-agnostic)

#### P0-30. Allocation API (allocate by fleet/pool/region, labels/selectors, fallback order, placement strategy)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (21h/21p/7m over 51 rows)
- **Competitors with it (7):** Agones/OM, Edgegap, Hathora, PlayFab, Pragma, SpacetimeDB, UGS Multiplayer
- **Why it matters:** Core DS flow.
- **Implementation sketch (musubi terms):** Exists: `POST /v1/server/allocate` (fleet id / pool+region). Gap: label selectors with ordered fallback, counter/list priorities, Packed/Distributed strategy.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Agones/OM: *`priorities[{type: Counter/List, key, order}]` → scale-down order and Packed tie-breaker* — allocation `priorities` rank candidates by counter/list; scale-down order still by age/idle
  - ✅ Agones/OM: *`GameServerAllocation` CR: ordered `selectors[]` with `matchLabels`, `matchExpressions` (In/NotIn/Exists/DoesNotExist)* — `selector` + ordered `selectors[]` fallback chain with `labels` and `match_expressions`; `matched_selector` in the response
  - ✅ Agones/OM: *`scheduling: Packed/Distributed` per allocation request* — `scheduling: packed|distributed` on every allocate request
  - ✅ Agones/OM: *`priorities[{type,key,order Ascending/Descending}]` to rank candidate servers by counter/list values* — `priorities: [{kind, key, order}]`
  - ✅ Agones/OM: *Label-locking alternative for high density (SDK `SetLabel` + label selector)* — `POST /v1/gameserver/label` + `labels`/`match_expressions` selectors
  - ✅ Agones/OM: *`spec.template.spec.nodeSelector` / tolerations / affinity to target node pools (e.g. Windows, GPU, spot)* — `spec.kubernetes {node_selector, tolerations, affinity, priority_class, service_account, host_network, termination_grace_secs, annotations}` on Pod spec / Agones template
  - ❌ Edgegap: *Memory allocation* — No memory field in `FleetSpec` (`rg memory_mib\|mem_limit` → none); docker/k8s providers don't set limits
  - 🟡 Agones/OM: *Lifecycle states `PortAllocation → Creating → Starting → Scheduled → RequestReady → Ready → Allocated/Reserved → Shutdo…* — States `starting / ready / allocated / in_game / shutdown / failed / terminated` (fleet/lib.rs, mig 0010/0067). No `Reserved`, no distinct `Unhealthy` (unhealthy servers are reaped by the 5 s reconciler, fleet/lib.rs `r…
  - 🟡 Agones/OM: *`spec.container` selects which container in a multi-container Pod is the game server* — `FleetSpec{command, image, env, working_dir, vcpu}` (fleet/lib.rs) — one process/container; k8s provider builds a single-container Pod (kubernetes.rs); no sidecars, volumes, node selectors, memory limits
  - 🟡 Agones/OM: *CRD `selectableFields` (status.state, status.nodeName) for server-side filtering (1.58)* — `GET /v1/server/fleets/{id}/servers` lists per fleet; no generic state/node filters documented
  - 🟡 Agones/OM: *`scheduling: Packed` (bin-pack onto fewest nodes; works with Cluster Autoscaler) vs `Distributed` (spread; for static/o…* — ssh provider picks the host with most free slots (README:ssh) = packing; no fleet-level policy, no Distributed mode, no node-aware scale-down ordering
  - 🟡 Agones/OM: *Cluster Autoscaler integration: Packed scheduling lets node pools shrink; docs for GKE/EKS/AKS autoscalers* — Node-level capacity is the operator's problem for ssh/firecracker; k8s provider Pods have no CA hints
  - 🟡 Agones/OM: *Selector `counters{key:{minCount,maxCount,minAvailable,maxAvailable}}` and `lists{key:{containsValue,minAvailable,maxAv…* — Only "has free player slots" (`players < capacity`); no arbitrary keyed thresholds, no containsValue
  - 🟡 Agones/OM: *`counters{key:{action Increment/Decrement, amount, capacity}}` and `lists{key:{addValues, deleteValues, capacity}}` app…* — Backfill reserves slots atomically (`players` increment) but nothing else is mutated
  - 🟡 Agones/OM: *`allocation.proto` `Allocate(AllocationRequest{namespace, multiClusterSetting, gameServerSelectors, scheduling, metadat…* — HTTP/JSON only (`openapi.rs`); no gRPC stubs
  - 🟡 Agones/OM: *Multi-cluster allocation: `GameServerAllocationPolicy` CR {priority, weight, connectionInfo{clusterName, allocationEndp…* — One musubi cluster spans regions (`region` per fleet, `POST /v1/server/allocate {pool, region?}` routes region-first, README:Georouting); no federation across independent control planes, no weighted cluster priorities
  - 🟡 Agones/OM: *Integration pattern: high-density servers — allocate `Allocated` server with free `rooms` counter capacity, fall back t…* — Works for one dimension (`players`); multi-room servers would need their own counter keys
  - 🟡 Agones/OM: *Integration pattern: player-capacity allocation through Lists (`players` list `minAvailable`)* — Same via `players/capacity` counter; no list of player ids on the server object
  - 🟡 Agones/OM: *Dedicated-node pinning for control plane (`requireDedicatedNodes`, priority classes, node selectors/tolerations/affinit…* — Helm chart has standard Deployment; no priority class / dedicated-node toggles
  - 🟡 Hathora: *Static process allocation* — `ScalePolicy{buffer, max, schedule, forecast}` keeps a ready pool per region (R233); no explicit min-old/max-total upgrade constraints — canary percent instead
  - 🟡 UGS Multiplayer: *Allocation API* — Relayed matches (`match_create` relayed, ≤64 players) are server-relayed over WS/QUIC/WebTransport/WebRTC on a musubi node; no standalone UDP relay allocation usable by an arbitrary transport (UTP) and no 100-connection…
  - 🟡 UGS Multiplayer: *Bespoke binary relay protocol over UDP* — musubi relay uses its own framed channels (`reliable_ordered/unordered`, `unreliable_sequenced/unreliable`, lz4) over QUIC/WS/WebTransport/WebRTC; no DTLS-over-raw-UDP mode for console/UTP clients.
  - 🟡 UGS Multiplayer: *Warm vs cold capacity; start-on-provision* — Scale-to-zero cold start exists; no warm-machine tier distinct from running servers.
  - 🟡 UGS Multiplayer: *Build configuration: launch params, configuration variables, server.json* — Env vars `MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY` + `GET /v1/gameserver` + `watch` long-poll (README 5.9); no file-based config injection or templated launch args.
  - 🟡 UGS Multiplayer: *Allocation TTL safeguard* — Health/readiness + shutdown endpoints; no idle-allocation TTL reaper (`rg -i ttl` in fleet → lease TTLs only).
  - 🟡 UGS Multiplayer: *Deallocation by process exit 0* — `POST /v1/gameserver/shutdown` explicit; exit-code semantics depend on provider (k8s restarts).
  - 🟡 UGS Multiplayer: *Game server lifecycle & failures* — Health states + watch + failover; crash restart via provider.
  - 🟡 UGS Multiplayer: *Game Server SDK (Unity, Unreal) + testbed* — HTTP game-server SDK only; no Unity/Unreal server plugin.
- **Already covered rows (21):** Agones/OM: Two-fleets (blue/green, A/B) update strategy with allocation preferen…; Agones/OM: Selector `gameServerState: Ready` or `Allocated` (allocate into an al…; Agones/OM: `metadata{labels, annotations}` patched onto the GameServer at alloca…; Agones/OM: Allocation batching: requests batched every `allocationBatchWaitTime`…; Agones/OM: Namespaced allocation: `gameservers.namespaces` Helm list; allocation…; Agones/OM: Integration pattern: canary fleet via `preferred` selector `canary: "…; Agones/OM: Integration pattern: matchmaker registration — server self-allocates…; Agones/OM: `GameServerAllocation` via `kubectl create -f` returns the allocated…; Agones/OM: Cold start when no Ready server exists: not supported — allocation re…; Edgegap: vCPU allocation; Hathora: CreateProcess (pre-allocation); PlayFab: Allocation: RequestMultiplayerServer (build id or build alias, prefer… …

#### P0-31. Real-money checkout / PSP integration (Stripe, Xsolla, PayPal, Adyen, hosted payment page, webshop, tax/invoices/refunds)  
*area:* economy/IAP · *musubi:* ❌ missing (4h/2p/5m over 11 rows)
- **Competitors with it (7):** AccelByte, brainCloud, LootLocker, Metaplay, PlayFab, Steamworks, UGS LiveOps
- **Why it matters:** Direct-to-consumer web shops bypass 30% store fees.
- **Implementation sketch (musubi terms):** Add `musubi-payments` (Stripe + Xsolla adapters, hosted checkout session → entitlement grant, refund/chargeback webhooks → revoke) and settlements ledger.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Payments (real money): payment station (hosted checkout, brandable), aggregators Adyen, Xsolla, Stripe, PayPal, AliPay,…* — no real-money PSP integrations (rg `stripe\|adyen\|xsolla` none) — only platform IAP
  - ❌ AccelByte: *Order (21) + Order(Dedicated) + Invoice (2) + Payment (9) + PaymentConfig (39) + PaymentStation (10) + PaymentAccount +…* — no order/payment stack
  - ❌ LootLocker: *Discord Commerce: sell Catalog Listings inside Discord (Stripe-backed checkout via bot)* — No Discord/Stripe commerce
  - ❌ Metaplay: *Google Play refunds: voided-purchases polling, `OnRefund` hook, refund revocation* — no refund/void handling (`rg -i 'refund\|voided'` README → none); entitlements revoke is manual
  - ❌ PlayFab: *Non-receipt payment processing: PayPal/Facebook/Steam/Xsolla via StartPurchase/PayForPurchase/ConfirmPurchase, real-mon…* — No real-money payment-provider checkout flow (only platform receipt validation); grep paypal/xsolla: none
  - 🟡 AccelByte: *Orders: order flow (create, pay, fulfil), order history, admin order search incl. payment processor reference id, refun…* — purchase endpoints record ledger entries; no order object lifecycle / refund / invoices / CSV
  - 🟡 Steamworks: *Settlement report* — purchases export/CSV via `musubi export`, webhooks on purchase events; no settlement/chargeback feed (depends on platform)
- **Already covered rows (4):** brainCloud: Transactions API; Steamworks: Refunds; Steamworks: Fraud guidance; UGS LiveOps: Fraud flags / refund handling

#### P0-32. Pools / profiles (ordered filtered subsets within a queue, hosting target per pool, large matches, team definitions)  
*area:* matchmaking · *musubi:* 🟡 partial (6h/6p/2m over 15 rows)
- **Competitors with it (7):** Agones/OM, brainCloud, Edgegap, EOS, GameLift, Rivet, UGS Multiplayer
- **Why it matters:** Flexible queue topologies.
- **Implementation sketch (musubi terms):** Add `pools[]` per queue (filters, hosting target, rules override) in musubi-matchmaker config.
- **What the reports found weaker/missing (merged rows):**
  - ❌ UGS Multiplayer: *A/B testing of rules* — `simulate` + stats only; no live traffic split between rule variants with outcome analytics.
  - 🟡 Agones/OM: *`ReleaseTickets(ids)` / `ReleaseAllTickets()` — return pending tickets to the pool* — Ready-check failure requeues the rest and penalises dodgers (mm/ready.rs); no operator "release all" endpoint
  - 🟡 Edgegap: *Example policies* — Per-fleet pools cover QA/pre-warm; MMO threshold via forecast; no owner-bound policy
  - 🟡 EOS: *Crossplay across EGS/Steam/consoles/mobile with unified Sessions/Lobbies pool; platform friend + Epic friend merge in S…* — musubi: crossplay flag in matchmaker/lobbies, platform friend import (/v1/friends/import), but no overlay UI
  - 🟡 GameLift: *Teams* — `Teams{count,size}` symmetric fixed teams balanced by MMR; parties kept together; no asymmetric/min-max per team
  - 🟡 GameLift: *Large matches (41–200 players)* — `max_count` unbounded in struct; relayed matches cap 64 (README); no large-match balanced algorithm
  - 🟡 UGS Multiplayer: *Pools (ordered filtered subsets within a queue) with own hosting + logic + timeout* — Ticket `query` strings (`+field`, `-field`, ranges) partition the population per ticket, but there is no server-side pool layering, no CEL (`rg -i '\bCEL\b'` → config comment only), no per-pool hosting target/timeout.
- **Already covered rows (6):** Agones/OM: `Pool{name, double_range_filters{double_arg,min,max,exclude NONE/MIN/…; Agones/OM: `MatchProfile{name, pools[], extensions}` — several pools per profile…; Agones/OM: Redis replication with separate read/write endpoints (`OM_REDIS_READ_…; brainCloud: Lobby type rules; Rivet: `registry.start({ endpoint, token, namespace, pool, runnerVersion, ma…; UGS Multiplayer: Hosting targets per pool

#### P0-33. Relayed (client-hosted) rooms/matches with host migration  
*area:* realtime/matches · *musubi:* 🟡 partial (4h/5p/1m over 10 rows)
- **Competitors with it (7):** AccelByte, Edgegap, Hathora, Meshing vendors, Photon, Steamworks, UGS Multiplayer
- **Why it matters:** Cheapest multiplayer for small teams.
- **Implementation sketch (musubi terms):** Exists: relayed matches ≤64 + host migration.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *P2P networking: AccelByte NetworkUtilities (ICE/TURN relay), host migration, latency & loss aware path selection (2026.…* — no client P2P relay/TURN
  - 🟡 AccelByte: *Session browser (deprecated) & P2P sessions via AccelByte NetworkUtilities (TURN/ICE relay), P2P host migration* — WebRTC DataChannel transport for client↔server; no client P2P relay/TURN service or P2P session type
  - 🟡 Meshing vendors: *Max entities* — musubi has per-cell `soft_cap` + spawn queue (202 + eta) and cost-based shedding, but no explicit hard world-wide entity cap knob (`rg soft_cap`)
  - 🟡 Meshing vendors: *Client-hosting (P2P)* — relayed matches with host migration but server-relayed; no Steam/EOS relay transport or embeddable server
  - 🟡 Photon: *Master Client* — relayed matches have host + host migration (2 files) with server-held snapshot; authoritative matches don't need it
  - 🟡 UGS Multiplayer: *Host migration (client-hosted)* — Relayed-match host transfer by RTT; authoritative matches fail over between nodes with snapshots (README) — but client-hosted state snapshotting is the game's job.
- **Already covered rows (4):** Edgegap: Host migration; Hathora: Relayed host migration for client-hosted matches; Steamworks: Owner / host transfer; Steamworks: Host migration in P2P

#### P0-34. Other language SDKs (Swift, Java/Kotlin, Dart/Flutter, Python, Go, Rust, Lua, Defold, Cocos, Haxe, GameMaker, Construct, Roblox, Discord Activities, WeChat)  
*area:* SDKs · *musubi:* ❌ missing (1h/4p/7m over 12 rows)
- **Competitors with it (6):** Agones/OM, Hathora, LootLocker, Metaplay, Pragma, Rivet
- **Why it matters:** Long tail of platforms.
- **Implementation sketch (musubi terms):** Priority order after Unity/Unreal/TS/C++: Rust/Go server SDKs (cheap from openapi), then Swift/Kotlin, Dart; community-maintained for the rest.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *Rust SDK: crate `agones`, async tokio, published to crates.io from 1.61* — No `musubi-gameserver-sdk` crate even though the backend is Rust
  - ❌ LootLocker: *GameMaker, Construct 3, GDevelop basic integrations* — none
  - ❌ Rivet: *Swift / SwiftUI client (iOS, macOS, tvOS, watchOS)* — `rg -i swift` = 0
  - 🟡 Metaplay: *MetaSerialization: attribute-driven tagged binary format (`[MetaSerializable]`, `[MetaMember(tagId)]`, `[MetaSerializab…* — musubi-idl: schema-defined typed state with generated codecs + postcard/JSON framing (README IDL row); not attribute-on-game-classes, no per-view visibility annotations
  - 🟡 Pragma: *Kotlin custom services & plugins compiled into the engine* — musubi extends via sandboxed WASM modules (RPCs, hooks, hot reload, canaries, fuel limits) — safer/faster to ship but no in-process JVM-level service authoring with direct DB access; no typed codegen of module RPCs into…
  - 🟡 Pragma: *Player Data service (schema-by-code)* — musubi has schema-less JSON storage collections (7 storage routes, JSON Patch, aggregates, full-text, collection contracts/indexes/live objects) + IDL types (`musubi-idl`) + WASM RPCs. No compile-time typed component mo…
  - 🟡 Rivet: *Rust SDK `rivetkit` (actors in Rust, beta) + Rust client `rivetkit-client`* — musubi guest ABI `bindings.rs` for WASM modules in Rust (README L369) + `musubi-sim-sdk` test client; no shipped Rust game-client crate
- **Already covered rows (1):** Metaplay: `GameOnInitialLogin` initialization hook, model constructor rules; st…

#### P0-35. Analytics export / warehouse connectors (S3, BigQuery, Snowflake, ClickHouse, Kafka, Amplitude)  
*area:* analytics/observability · *musubi:* 🟡 partial (0h/6p/0m over 7 rows)
- **Competitors with it (6):** AccelByte, Beamable, Metaplay, Hiro/Satori, Rivet, UGS LiveOps
- **Why it matters:** Data teams own the warehouse.
- **Implementation sketch (musubi terms):** Add sink framework in musubi-integrations: batched Parquet/JSONL to S3-compatible + ClickHouse/Kafka writers for `analytics_events` and domain events.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *Data Connector: S3 export of service + game telemetry (flush interval/size); AIS add-on (Aurora/Athena raw events, flex…* — events/telemetry stored in Postgres with webhooks + event bus export (README "Events"); no S3/warehouse connector, no Parquet (rg `parquet\|s3` export: none)
  - 🟡 Beamable: *Segmentation / cohorts* — Named audiences w/ rule expressions + membership routes; no CSV bulk tagging UI/import
  - 🟡 Beamable: *Microservice logging (OTel)* — Modules: structured logs + OpenTelemetry traces/metrics exported by operator; no hosted log search UI, no per-request log-level rules
  - 🟡 Metaplay: *Analytics dispatcher + sinks: built-in JSON-to-S3 sink, BigQuery sink (flattened schema), custom sinks; batching 100 ev…* — events go to Zanshin or HMAC webhooks with backoff/dead-letter; no S3/BigQuery sink, no warehouse schema
  - 🟡 Hiro/Satori: *Data lake export: BigQuery (partitioned by receive time, clustered), Snowflake, Redshift, S3, Databricks (Parquet via S…* — musubi `musubi export` JSONL dump + `GET /v1/server/events` cursor stream + webhooks; no warehouse connectors
  - 🟡 Rivet: *Advanced ClickHouse analytics* — analytics events + Zanshin LiveOps feed (README L191), no ClickHouse pipeline (`rg -i clickhouse` = 0)

#### P0-36. Meta Quest / Oculus, Viveport, other platform IdPs  
*area:* auth · *musubi:* ❌ missing (0h/0p/8m over 8 rows)
- **Competitors with it (6):** brainCloud, LootLocker, Photon, PlayFab, Pragma, UGS LiveOps
- **Why it matters:** VR and regional storefronts require their first-party identity.
- **Implementation sketch (musubi terms):** Add verifiers (Meta nonce `user_id` check, Viveport token) in musubi-auth; regional stores as OIDC presets.
- **What the reports found weaker/missing (merged rows):**
  - ❌ brainCloud: *Oculus / Meta Horizon* — no Oculus/Meta provider in `musubi-auth` (grep `oculus` only in matchmaker tests) — would need generic OIDC/custom-auth script
  - ❌ LootLocker: *Meta / Oculus auth (user id + nonce, app id/secret)* — rg -i 'oculus\|meta.*nonce' = 0 hits; no Meta nonce verification route
  - ❌ Photon: *Oculus/Meta auth* — rg oculus=0
  - ❌ Photon: *Viveport auth* — rg viveport=0
  - ❌ PlayFab: *LoginWithKongregate + Link/Unlink, GetPlayFabIDsFromKongregateIDs* — None (grep kongregate: 0)
  - ❌ Pragma: *Meta Quest identity provider (0.6.0)* — `rg -i 'quest\|oculus'` (auth) → none.
  - ❌ UGS LiveOps: *Oculus / Meta Quest* — `rg -i oculus` → 0 hits; would need Meta user-proof validation provider

#### P0-37. Multi-tenant / multi-title / namespace model (publisher + game namespaces, environments)  
*area:* auth · *musubi:* 🟡 partial (5h/7p/3m over 16 rows)
- **Competitors with it (6):** AccelByte, Beamable, brainCloud, Metaplay, PlayFab, Rivet
- **Why it matters:** Publishers run several titles on one backend with shared accounts.
- **Implementation sketch (musubi terms):** Exists: `[[projects]]` multi-project. Gap: publisher→game account hierarchy with shared social graph and per-env promotion.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Country deny list (ISO 3166-1) blocks registration & login; publisher namespace* — no geo/country blocking (rg `deny_list\|country` → only location field)
  - ❌ brainCloud: *Parent/child profiles (multi-app publisher accounts)* — musubi `[[projects]]` isolates games per project; no cross-project parent/child profile hierarchy or shared parent wallet
  - ❌ Rivet: *CDN sites with custom domain + SSL, namespace CDN auth (basic/user)* — no static-site/CDN hosting (`rg -i cdn` = country header only)
  - 🟡 AccelByte: *Profile update strategy per field (admin upsert, public read) & input validation rules API (InputValidations, 10 ops) i…* — static password policy + name filter in config; no per-field strategy API
  - 🟡 AccelByte: *Custom permissions for Extend apps in Public Cloud (`CUSTOM:ADMIN:NAMESPACE:{ns}:RESOURCE` / `CUSTOM:NAMESPACE:{ns}:USE…* — server-key scopes + CEL policies; no per-resource permission strings for module RPCs
  - 🟡 Beamable: *Account Id vs Player Id (realm-scoped gamertag)* — One account id per project; no cross-project global identity
  - 🟡 Beamable: *Realms (environments) management & archiving* — Projects/environments + config via files/admin API; no realm hierarchy or archive concept
  - 🟡 Beamable: *Realm archiving & child realm creation* — Environments are static config
  - 🟡 Metaplay: *Runtime options: YAML base + per-environment files, CLI/env overrides, hot reload, typed options classes with validatio…* — TOML config + env overrides, `musubi config`; no hot reload of options, no typed game-defined option classes
  - 🟡 PlayFab: *Entity tokens: EntityToken per entity type (title, master_player_account, title_player_account, character, group), GetE…* — musubi: JWT access + refresh rotation per account/session; server-key roles for title-level auth; no separate master-vs-title-player identities (multi-project via `[[projects]]` shares one account scope) and no characte…
- **Already covered rows (5):** AccelByte: Namespaces: publisher (globally unique, unified accounts across games…; Beamable: CID-scoped (customer) logins for admin tools (`customerScoped:true`); Beamable: Realm secret (UUID) for signed requests; `config secret`; `generate-e…; PlayFab: Throttling details: per-API limits configurable in Title Settings → L…; Rivet: Admin token (self-host, static, all namespaces) `RIVET__AUTH__ADMIN_T…

#### P0-38. Voice chat (positional, channels, PTT, mute matrix, moderation, recording)  
*area:* chat · *musubi:* ❌ missing (2h/1p/18m over 26 rows)
- **Competitors with it (6):** EOS, Photon, PlayFab, Rivet, Steamworks, UGS Multiplayer
- **Why it matters:** Studios expect first-party voice.
- **Implementation sketch (musubi terms):** Only Aurix token minting (`/v1/voice/token`). Deeper Aurix integration (rooms per lobby/party, mute/kick, moderation hooks) or a WebRTC SFU sidecar authenticated by musubi-auth.
- **What the reports found weaker/missing (merged rows):**
  - ❌ EOS: *EOS Voice (RTC Audio): hosted media servers, rooms up to 64 participants (16 for lobby voice), two integration modes: L…* — no voice media server; only Aurix third-party token minting (/v1/voice/token, /v1/voice/report)
  - ❌ Photon: *Photon Video 2* — nothing (rg video=0)
  - ❌ Photon: *Photon Voice 2 (Unity)* — no voice media server; README delegates voice to external Aurix (voice endpoints only hand out tokens)
  - ❌ PlayFab: *Party accessibility: speech-to-text, text-to-speech voice profiles, UX guidelines, Xbox TCUI* — No STT/TTS (grep speech: none)
  - ❌ UGS Multiplayer: *Non-positional (2D) voice channels* — No voice engine; `POST /v1/voice/token {party_id / match_id}` mints Aurix tokens for `party-<id>`/`match-<id>` channels (README "Integrations").
  - ❌ UGS Multiplayer: *Server-side recording (SSR)* — Match recording/replay exists for game data (README), not voice.
  - 🟡 EOS: *RTC-data: arbitrary data messages inside a voice room (~500 msg/s limit, max msg size, per-participant targeting)* — musubi party/match data channels over WS/QUIC (PartyDataSend, MatchDataSend) cover the use case without voice
- **Already covered rows (2):** Rivet: Voice (Aurix) token minting; UGS Multiplayer: Presence / buddies (Vivox-native)

#### P0-39. Managed relay / IP-hiding gateway / DDoS protection for game servers  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (1h/1p/4m over 7 rows)
- **Competitors with it (6):** Agones/OM, Colyseus, GameLift, Hathora, Photon, Rivet
- **Why it matters:** Attack mitigation is a studio must-have.
- **Implementation sketch (musubi terms):** Reference relay tier on musubi-edge (UDP) with per-player connect tokens; document anycast + provider scrubbing.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *Flowtriq DDoS detection labelling attacked GameServers via SDK REST* — Anticheat signals exist (README:anticheat) but no DDoS labelling integration
  - ❌ Hathora: *Trusted client IPs per room (DDoS allow-list)* — `rg allowlist/trusted_ip` → only `[metrics] allow_ips` for admin endpoints; UDP edge uses per-player connect tokens instead but no allow-list throttling mode
  - ❌ Photon: *DDoS protection* — nothing in repo (rg ddos=0); edge has rate limiting only
  - 🟡 Rivet: *Game Guard: DDoS mitigation (IP allow-list per lobby, max connections per IP, L3/L4 filtering, L7 rate limiting), TLS t…* — `musubi-edge` TLS + auth + per-key/IP rate limits (`limits.rs`); no L3/L4 scrubbing or per-lobby IP allow-listing (`rg -i ddos` = 0)
- **Already covered rows (1):** Rivet: Regions: actor pinned to creation region for life; auto nearest-regio…

#### P0-40. Multi-environment / namespaces (dev/stage/prod, preview per PR, realm archive, promotion)  
*area:* deployment/ops · *musubi:* 🟡 partial (4h/7p/3m over 15 rows)
- **Competitors with it (6):** AccelByte, Colyseus, Hathora, PlayFab, Rivet, UGS Multiplayer
- **Why it matters:** Safe release pipeline.
- **Implementation sketch (musubi terms):** Exists: `[[projects]]` + `musubi export/import` + content bundle environments. Add first-class environments inside one control plane with promotion and PR-preview namespaces.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Rivet: *Preview deployments: namespace per PR auto-created/destroyed, PR comment link, Vercel preview-namespace action* — `rg -i "preview\|pull request"` = 0 in deploy tooling
  - ❌ Rivet: *Dashboard (dashboard.rivet.dev + self-host `/ui`): actors list/search by ID, state view/edit, connections, events, logs…* — README L8: "admin console not written yet"; only JSON admin API + `musubi-integrations/console.rs` (external console roles)
  - ❌ Rivet: *MCP server (`https://mcp.rivet.dev/mcp` + local `@rivet-dev/mcp`): list actors, call actions, open Inspector, manage Co…* — `rg -i mcp` = 0
  - 🟡 AccelByte: *Access Logs: every API/WS interaction searchable 30 days by time/namespace/service/user/client/method/status/path/trace…* — request recording/replay + tracing via OpenTelemetry; no searchable per-request access-log UI/API
  - 🟡 AccelByte: *Audit logs: config change tracking across namespaces, filter by category/action/object type/object id/actor (user/app),…* — admin audit log of mutations (`/v1/admin/audit`); no comments on entries, no object-type taxonomy
  - 🟡 Hathora: *Entity hierarchy Team > Application > Build/Fleet > Deployment > Process > Room* — musubi has fleets > builds > servers (R194, R233) and multi-environment via separate deployments/`musubi export/import`; no "application/environment" namespace object inside one cluster
  - 🟡 Rivet: *Worker pools per namespace (`normal` / `serverless`), `runner_name_selector`, pool list/delete CLI* — fleets per region/provider (`POST /v1/server/fleets`) play this role for dedicated servers; WASM modules run on every node, no pool selection
  - 🟡 Rivet: *Namespaces isolate actors, worker names, pools, tokens, config; per env / per customer / per PR; `--create-namespace`* — one game per deployment; `[cluster] tenant` / key scopes (`musubi-core/config.rs`, README L321) but no first-class multi-namespace isolation in one control plane
  - 🟡 Rivet: *AI-/user-generated actor code deployed to sandboxed namespaces* — `module canary --accounts` isolates a module version to accounts; no sandboxed tenant namespaces
  - 🟡 Rivet: *GitHub Actions deploy workflow via `rivet setup-ci`; preview namespaces per PR* — `musubi deploy` manifest is CI-friendly (README L35) but no generated workflow
- **Already covered rows (4):** Colyseus: Custom `logger` (alias of `console`; pino/winston examples); `DEBUG=c…; PlayFab: Throttling model: fixed-window rate limits, keys = IP for anonymous A…; Rivet: No upper bound on actors per namespace; ~1,000 concurrent actors guid…; UGS Multiplayer: Multi-environment projects

#### P0-41. Rules engine / triggers / automation (event → action with filters, scheduled tasks, no-code)  
*area:* progression/LiveOps · *musubi:* 🟡 partial (7h/12p/1m over 20 rows)
- **Competitors with it (6):** Beamable, LootLocker, Metaplay, PlayFab, Pragma, UGS LiveOps
- **Why it matters:** LiveOps teams act without engineering.
- **Implementation sketch (musubi terms):** Exists: WASM hooks + webhooks + cron schedules. Add declarative `triggers` (event type + CEL/JSONPath filter → reward/flag/webhook/push action) stored in DB and editable at runtime (musubi-runtime/triggers.rs).
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Claimable Links: shareable URL + QR PNG per trigger, start/end dates, hosted claim page with UPA auth* — Coupons (`/v1/store/coupons`) are in-game codes only; no hosted claim page/QR
  - 🟡 Beamable: *Player activities (install/last session/last purchase date)* — Audiences can filter on account fields; no built-in campaign scheduler relative to activity dates (would need Zanshin)
  - 🟡 Beamable: *Scheduled job retry policy & executions* — Workflows have retries/backoff and history; schedules lack an execution-history/upcoming-times API
  - 🟡 Metaplay: *Dashboard actions: server actions triggered from dashboard with generated modal UI, default values, custom audit entries* — admin can call WASM RPC via `/v1/rpc/{id}` with server key; no generated form UI
  - 🟡 PlayFab: *PlayStream Rules: trigger on any PlayStream event (incl. custom), conditions on event payload, actions: grant VC/item,…* — musubi: event bus → webhooks + WASM hooks with full host API; no no-code rule editor with built-in actions (requires writing a module)
  - 🟡 PlayFab: *Actions & Rules: one trigger (PlayStream event type incl. custom), conditions on payload, multiple actions (CloudScript…* — musubi hooks/webhooks need code; no declarative rule engine (see Automation rows)
  - 🟡 PlayFab: *CloudScript Event Hub functions (RegisterEventHubFunction/ListEventHubFunctions, 2024) — route PlayStream events to Azu…* — Event bus delivers to HTTP webhooks only (grep kafka/nats: none); no message-bus sink
  - 🟡 PlayFab: *Azure Functions integration: RegisterHttpFunction / RegisterQueueFunction / RegisterEventHubFunction, ListFunctions, Un…* — Webhooks (push events out) + module RPC; no managed "register external function and call it as RPC with player context" bridge (would need module shim)
  - 🟡 PlayFab: *Rule actions catalog: ExecuteCloudScript / ExecuteFunction, SendEmail (template), SendPushNotification (template), Gran…* — Equivalent effects reachable from module hooks/webhooks only
  - 🟡 UGS LiveOps: *Event → action triggers (Cloud Code script/module or Webhook)* — Equivalent plumbing is split: module hooks (`hook_authenticated`, `hook_leaderboard_reset`, `before_chat_message`, …) and webhooks with filters (`/v1/admin/webhooks`); no declarative "run RPC X when event Y matches filt…
  - 🟡 UGS LiveOps: *Debug & monitor events (logs per trigger)* — Webhook deliveries list + audit; no per-trigger log UI
  - 🟡 UGS LiveOps: *Authoring: dashboard, CLI (`ugs triggers`), REST Admin API (600 req/min per SA)* — Webhooks via REST + deploy manifest; hooks in code
  - 🟡 UGS LiveOps: *RBAC roles (`Triggers Configuration Viewer/Editor`)* — Key roles `admin/ops`; not per-service
- **Already covered rows (7):** PlayFab: Webhooks from PlayStream/rules to external HTTP (custom headers), Azu…; Pragma: Data rights: irreversible account deletion; Pragma: Order sync / processing trigger; UGS LiveOps: Script types: API vs Moderation (trigger-only); UGS LiveOps: Key saved event (Triggers); UGS LiveOps: Triggered (event-driven) pushes; UGS LiveOps: Schedules as events feeding Triggers (payload, versioned)

#### P0-42. Reconnect / resumable sessions / WS migration & hibernation  
*area:* realtime/matches · *musubi:* 🟡 partial (6h/4p/2m over 12 rows)
- **Competitors with it (6):** AccelByte, Colyseus, Hathora, Pragma, Rivet, UGS Multiplayer
- **Why it matters:** Mobile networks drop; connections must survive failover.
- **Implementation sketch (musubi terms):** Exists: resumable sessions with sequence replay. Gap: live WS migration across node failover/upgrade (Rivet) — hand sockets over via `musubi-edge` connection registry.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *Room reference: `state`, `sessionId`, `roomId`, `name`, `reconnectionToken`, `reconnection`, `clock`; `onStateChange`,…* — no client SDK
  - 🟡 Colyseus: *`exposedMethods` to restrict which matchmaking calls clients may use (create/join/joinById/joinOrCreate/reconnect)* — relayed `match_create` can be disabled per project? (`rg allow_client_create` — not found); authoritative matches are server/matchmaker-created only; access policies (CEL-like) gate HTTP routes
  - 🟡 Rivet: *Hooks: `onMigrate`, `createState`, `onCreate`, `createVars`, `onWake`, `run`, `onSleep`, `onDestroy`, `onBeforeConnect`…* — module exports `init`, `join`, `leave`, `input`, `tick`, `end`, `snapshot`/`restore` (README "Module ABI"); no migrate/wake/sleep hooks because actors don't sleep
  - 🟡 Rivet: *Client reconnect with automatic resubscribe; message buffering while disconnected* — server supports `session_resume`; Unreal plugin lacks auto-reconnect (`sdk/unreal/README.md`), no official client to buffer
  - 🟡 UGS Multiplayer: *Reconnect after disconnect with grace period* — WS sessions are resumable (subscriptions restored, README "sessions resumed") but lobby membership has no disconnected state / removal timer / auto host migration on host disconnect.
- **Already covered rows (6):** AccelByte: Login Queue: max concurrency, logins/sec, margin %, deactivation cool…; Colyseus: Development mode `devMode`: rooms cached & restored across server res…; Colyseus: Security defaults: better-call `/api/reference` OpenAPI page disabled…; Hathora: Client transport negotiation, resumable sessions, heartbeats/RTT; Pragma: Reconnection to running game; Rivet: Connection params / `getParams` (fresh per connect), `createConnState…

#### P0-43. Hosted login / account portal (web pages for sign-in, reset, link, consent)  
*area:* auth · *musubi:* ❌ missing (1h/2p/5m over 9 rows)
- **Competitors with it (5):** Beamable, EOS, LootLocker, Pragma, UGS LiveOps
- **Why it matters:** Web/launcher flows and cross-platform linking need a page the game can open.
- **Implementation sketch (musubi terms):** Missing. New `musubi-portal` (static HTML served by musubi-edge) over existing auth routes: login, reset, link, consent, 2FA; OAuth2 authorization-code flow for third-party web apps.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Hosted sign-in / reset-password / confirm-account pages at auth.game, branded (logo, name)* — No hosted auth web pages in musubi; magic link + reset are API-only (rg -i 'hosted' = no auth pages)
  - ❌ LootLocker: *Remote Login: game shows 8-digit code, player authenticates at auth.game on another device (device-code flow), game pol…* — rg -i 'device_code\|remote login' = 0 hits
  - ❌ LootLocker: *External Provider Linking via auth.game: begin link lease → URL, poll 2–3s, transfer provider when it already exists on…* — No lease/poll web flow; musubi link routes require client-side provider token
  - ❌ LootLocker: *Hosted Player Portal for players (profile, manage linked accounts, review survey answers, standalone surveys offered th…* — No player-facing hosted web pages in musubi
  - ❌ Pragma: *Social Player Portal* — no player-facing web portal.
  - 🟡 EOS: *Verifiable parental consent (VPC): under-13/age-of-digital-consent players must supply guardian email; Epic emails guar…* — musubi: age brackets + parental consent token flow (/v1/account/parental/*, [compliance]) but no hosted guardian email/verification workflow
  - 🟡 UGS LiveOps: *Unity Player Accounts (Unity-hosted web identity, email/social login hosted by Unity)* — musubi hosts email+password / magic-link / passkey accounts itself (`/v1/auth/email/*`, `/v1/auth/magic/*`, `/v1/auth/passkey/*`) but has no hosted web sign-in page / portable cross-game account service
- **Already covered rows (1):** Beamable: HTML / web sign-in page guide

#### P0-44. Lobby attributes & search (typed public/private attrs, numeric ops, sort, distance, per-member data)  
*area:* matchmaking · *musubi:* 🟡 partial (5h/5p/0m over 10 rows)
- **Competitors with it (5):** AccelByte, EOS, Photon, Steamworks, UGS Multiplayer
- **Why it matters:** Server browsers and custom games need rich filtering.
- **Implementation sketch (musubi terms):** Extend lobby model: `attributes` (typed, public/private, per-member) + indexed JSONB with GT/LT/IN/NEAR operators and sort in `GET /v1/lobbies`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 EOS: *Lobby attributes + member attributes (key/value typed bool/int64/double/string), public/private visibility per attribut…* — lobby metadata JSON ≤16KB + per-member ready/team; no per-member free-form attributes or per-attribute visibility (rg "member_metadata" → none)
  - 🟡 EOS: *Session search by id / user id / attributes with EQ/NE/GT/GTE/LT/LTE/DISTANCE/ANYOF/NOTANYOF/CONTAINS, reserved attrs (…* — GET /v1/matches?label/query (match listing with label query) — fewer operators, no reserved attr constants
  - 🟡 EOS: *Session attributes typed (bool/int64/double/string) with public/private visibility (advertisement type)* — match label/metadata JSON; no per-attribute visibility
  - 🟡 Steamworks: *Search filters* — metadata equality + region + open-slot filters (README Lobbies); no numeric comparison operators or near-value sort on lobby metadata
  - 🟡 UGS Multiplayer: *Public lobby query* — `GET /v1/lobbies` filters: name prefix, region, metadata containment, has_slots. No numeric range operators on custom fields, no sort orders, no random sampling, no indexed typed fields.
- **Already covered rows (5):** AccelByte: Game session attributes with optimistic versioning (version mismatch…; AccelByte: Lobby admin API: CCU count, bulk blocked players, player session attr…; EOS: Sessions Interface: owner (player or dedicated server via trusted cli…; Photon: SessionProperty typing; Steamworks: Lobby search

#### P0-45. Player reports / incidents workflow (categories, evidence packages, queue, moderator actions)  
*area:* moderation/anti-cheat · *musubi:* ❌ missing (2h/1p/3m over 9 rows)
- **Competitors with it (5):** EOS, LootLocker, Metaplay, PlayFab, UGS Multiplayer
- **Why it matters:** Moderators need queues, not raw rows.
- **Implementation sketch (musubi terms):** Exists: `/v1/reports`, `/v1/admin/reports*`, chat reports. Gap: incident objects with evidence packages (chat/voice excerpts), SLA states.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Metaplay: *Player incident reports: client logs + stack traces + env info bundled for desyncs/network issues; deep link to Grafana…* — musubi has `/v1/reports` (player reports of other players) and anti-cheat, not client crash/incident upload
  - ❌ UGS Multiplayer: *Moderation dashboard (UDash)* — No console.
  - 🟡 UGS Multiplayer: *Incident workflow* — `player_reports` + `reports` routes, bans/shadow bans/mutes, audit log (README "Reports, bans"). No incident aggregation, assignee/status workflow, risk score, text reputation or evidence bundling.
- **Already covered rows (2):** EOS: Player Reports Interface: SendPlayerBehaviorReport (reporter, reporte…; LootLocker: UGC lifecycle: Asset Candidates (metadata + files), mark complete, op…

#### P0-46. Campaigns / operator messaging (mail/push/email templates, targeting, schedules, localization, journeys)  
*area:* progression/LiveOps · *musubi:* ❌ missing (2h/2p/4m over 9 rows)
- **Competitors with it (5):** Beamable, brainCloud, LootLocker, Hiro/Satori, PlayFab
- **Why it matters:** Retention marketing inside the backend.
- **Implementation sketch (musubi terms):** Add `campaigns` (audience + channel template + schedule/recurrence + per-language bodies) on top of push/mail/notifications; journeys as workflow templates.
- **What the reports found weaker/missing (merged rows):**
  - ❌ brainCloud: *LiveOps Campaigns (6.0, Enterprise)* — feature flags/live events come from Zanshin (`/v1/liveops/flags`, `/live-events`); no A/B experiment engine, allocation or significance analytics in musubi itself
  - ❌ LootLocker: *Distribute Platform Keys via Discord commands/campaigns (gated giveaways, creator keys)* — No platform key inventory
  - ❌ LootLocker: *External Reward Campaigns (Jun 2026): campaigns granted via external claim (link/Discord), Announce Campaign endpoint* — No hosted claim/campaign surface
  - ❌ Hiro/Satori: *Escalating offer chain recipe: journey + live-event participation window + Hiro store overrides + revert* — none
  - 🟡 Beamable: *Email campaigns (SMTP via Beamable) & `EmailContent` templates* — SMTP email for auth flows only (`musubi-integrations/email.rs`); no marketing email
  - 🟡 Beamable: *Campaigns (mass communication)* — Mail w/ audience targeting + push templates/scheduling + SMTP exist as APIs; no campaign entity/wizard, no email campaigns to segments
- **Already covered rows (2):** Beamable: Player Stats for campaigns; Hiro/Satori: Outbound webhooks

#### P0-47. Client prediction / reconciliation / interpolation library  
*area:* realtime/matches · *musubi:* ❌ missing (1h/3p/4m over 14 rows)
- **Competitors with it (5):** AccelByte, Colyseus, Meshing vendors, Photon, UGS Multiplayer
- **Why it matters:** Smooth feel at 60–150 ms RTT.
- **Implementation sketch (musubi terms):** Missing client half. Ship `musubi-predict` (TS/C#) reconciler + interpolation on top of `match_state_delta` tick buffer; server already provides rollback inputs.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *Learning material: step-by-step tutorials (Phaser ×4 incl. client-predicted input/fixed tickrate/interpolation, Unity ×…* — README + `docs/` only; no engine tutorials/examples (`examples/` dir: `rg --files examples \| head` → only module samples)
  - ❌ Meshing vendors: *Interpolation / extrapolation* — client-side concern; no SDK helper (Unreal scaffold has none, `rg interpolat` 2 comment hits)
  - ❌ Meshing vendors: *Client-side prediction* — server supplies ticks and lag-comp rewind; no client prediction helper
  - ❌ Photon: *Snapshot interpolation* — client-side
  - 🟡 AccelByte: *Session admin: global configuration, DSMC default config, env vars, templates (11), game session (30), party (20), stor…* — lobbies/parties/storage have; recent players list: missing; max-active-sessions-per-player: partial
  - 🟡 Photon: *Time synchronization* — server tick/RTT in protocol; client clock sync not provided
  - 🟡 Photon: *Lag compensation (PUN)* — server-side rollback + tick hashes in authoritative matches (README rollback); no hitbox rewind library
- **Already covered rows (1):** Meshing vendors: Deterministic rollback (GGPO)

#### P0-48. Announcements / Title News / MOTD / broadcasts (localized, scheduled)  
*area:* social · *musubi:* 🟡 partial (3h/7p/2m over 13 rows)
- **Competitors with it (5):** Beamable, LootLocker, Metaplay, PlayFab, Steamworks
- **Why it matters:** Communicate with the whole player base.
- **Implementation sketch (musubi terms):** Add `news` collection + `GET /v1/news?lang` with schedule and per-language bodies (musubi-social).
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Push Broadcasts to a Discord channel from the Broadcast editor* — No Discord publishing (webhooks are generic HTTP only)
  - ❌ PlayFab: *Title News: entries with title/body/timestamp, localized per language, GetTitleNews client API, created via Game Manage…* — No title news/MOTD endpoint (grep news: none); closest = typed configs
  - 🟡 Beamable: *Content service (typed content objects)* — Content bundles with versions/diff/rollback + IDL typed configs `/v1/configs`; no engine-side typed ContentObject tooling, no client manifest push/cache (no SDK)
  - 🟡 Beamable: *Profanity filtering / moderation* — Chat word filters + automod webhook exist; no built-in OpenAI moderation provider, no filtering of group names/display names
  - 🟡 Beamable: *Group tag (≤3 chars) & MOTD/slogan moderated* — Tag/description fields exist; no moderation on them
  - 🟡 Beamable: *Announcements* — Mail + notifications + live events cover; no dedicated announcement content type with prefab
  - 🟡 Beamable: *Announcements: read/claim, raw, attachments, list/search* — Mail claim/read; announcements not a separate entity
  - 🟡 Metaplay: *Broadcasts: scheduled start/end, audience targeting, localized content, details/stats (received counts), edit/delete, d…* — `POST /v1/admin/mail/broadcast` to audience exists; no scheduled window with on-login delivery semantics, no per-broadcast seen stats
  - 🟡 Metaplay: *Localized content in mails/broadcasts/push (`LocalizedString` with per-language fallback)* — push templates per locale (README push row); mail bodies are opaque JSON
- **Already covered rows (3):** Beamable: Groups (guilds) create/join/leave; Steamworks: Lobby chat / binary broadcast; Steamworks: News / announcements feed

#### P0-49. Legal agreements / ToS / privacy policy versioning and acceptance tracking  
*area:* compliance · *musubi:* ❌ missing (2h/0p/4m over 7 rows)
- **Competitors with it (4):** AccelByte, EOS, LootLocker, Meshing vendors
- **Why it matters:** Store and legal requirement.
- **Implementation sketch (musubi terms):** Add `legal_documents` (version, locale, mandatory) + `account_consents` + login gate (`403 consent_required`) in musubi-account.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Legal agreements service: ToS/Privacy/EULA/marketing-preference documents, versions, per-country/locale, publisher-wide…* — no legal-document/consent-version service (rg `eula\|terms_of_service` none); only parental consent token
  - ❌ EOS: *Auth scopes (BasicProfile, FriendsList, Presence, Country) with user consent management* — musubi has no OAuth-style scope/consent model for third-party apps (operator scoped keys only)
  - ❌ EOS: *Consent dialog: app name, 128x128 logo, privacy policy URL, per-scope permissions; users can revoke access outside the…* — no per-app consent/scope model in musubi (single first-party trust domain)
  - ❌ LootLocker: *Custom Signup Fields (extra questions, ToS/Privacy acceptance) stored as player metadata* — No signup form schema; studio would store answers in storage manually
- **Already covered rows (2):** Meshing vendors: Self-hosting license; Meshing vendors: Local multi-machine deployment

#### P0-50. Per-server metrics (CPU/mem/net/egress/OOM/active connections time series)  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (0h/2p/4m over 7 rows)
- **Competitors with it (4):** Edgegap, GameLift, Metaplay, UGS Multiplayer
- **Why it matters:** Right-sizing and billing.
- **Implementation sketch (musubi terms):** Add cadvisor/docker-stats scrape per gameserver into Prometheus labels + `GET /v1/server/gameservers/{id}/metrics?start&end&step`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *Container metrics API* — Prometheus `musubi_fleet_*` cluster-level gauges only; no per-server CPU/mem/net
  - ❌ Edgegap: *OOM insights* — No memory limits or OOM tracking for game servers
  - ❌ Edgegap: *Egress metric* — `rg egress` → only docs mentions
  - ❌ Metaplay: *Game server Metrics page in dashboard (business + technical charts, `api.metrics.view`, implementation descriptions), c…* — Prometheus only
  - 🟡 GameLift: *Container metrics* — via k8s/cAdvisor, not musubi
  - 🟡 UGS Multiplayer: *Fleet / server analytics* — Prometheus metrics + per-build vCPU-hour usage endpoint (README "usage"); no dashboard.

#### P0-51. Declarative rule DSL / rule sets (comparison, distance, collection, compound, sort rules; typed attributes)  
*area:* matchmaking · *musubi:* 🟡 partial (5h/9p/2m over 17 rows)
- **Competitors with it (4):** AccelByte, Edgegap, GameLift, UGS Multiplayer
- **Why it matters:** Designers tune matchmaking without redeploying code.
- **Implementation sketch (musubi terms):** Add a rule-set JSON DSL (`rules: [{type, measurements, operation, reference, expansions}]`) evaluated by musubi-matchmaker in addition to the fixed structs; typed attributes on tickets.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *Matchmaking X-Ray: per-ticket timeline/history, ruleset applied, pool overview, filters, refresh; metrics dashboards* — matchmaker metrics p50/95/99 + match journal; no per-ticket explain/X-ray UI
  - 🟡 AccelByte: *match2 admin: Config (5), Play Feature Flag (kill-switch for matchmaking per namespace), X-Ray config, environment vari…* — queues/rulesets/backfill have; no play feature flag kill switch (maintenance mode global only)
  - 🟡 GameLift: *CloudFormation / Cloud Control / Terraform* — Helm chart + Terraform profiles (k8s/Hetzner/OVH) deploy musubi itself; fleets/queues configured via API/`musubi fleet`, no declarative resource provider
  - 🟡 GameLift: *API families count* — 26 fleet/gameserver/matchmaker routes in `ROUTES` (`/v1/server/fleets*` 12, `/v1/gameserver*` 9, `/v1/server/matchmaker*` 4, `/v1/server/gameservers/{id}`)
  - 🟡 GameLift: *Rule types* — fixed rules: MMR window, ping window, roles quota, cross-play groups, avoid lists, no-repeat opponents, query filters (+field:value ranges wildcards); no compound/collection/sort rules
  - 🟡 GameLift: *Rule set examples (11)* — README matchmaking examples (queue configs)
  - 🟡 GameLift: *Validate rule set* — `PUT /v1/server/matchmaker/queues/{name}` validates struct; `simulate` for behaviour
  - 🟡 GameLift: *Map / game-mode voting via collection rules* — query `+mode:x` exact filters only
  - 🟡 UGS Multiplayer: *Declarative rule DSL* — MMR/ping windows, fixed teams balanced by MMR, role quotas, cross-play groups (README "Matchmaking"); arbitrary logic only via WASM `matchmaker_score/propose` hooks — no JSON rule language a designer can edit.
- **Already covered rows (5):** AccelByte: Matchmaking v2 (separate microservice from Lobby; v1 legacy via Lobby…; Edgegap: Backfill; GameLift: Matchmaking simulation; GameLift: Matchmaking roadmap for GameLift hosting; GameLift: Describe / delete configurations & rule sets

#### P0-52. Feature flags / remote config variants with targeting (rollout %, conditions, scheduled overrides)  
*area:* progression/LiveOps · *musubi:* ❌ missing (0h/3p/5m over 8 rows)
- **Competitors with it (4):** Beamable, LootLocker, Metaplay, UGS LiveOps
- **Why it matters:** Ship dark, turn on safely.
- **Implementation sketch (musubi terms):** Exists: Zanshin flags proxied (`/v1/liveops/flags`). Add native flag evaluation in core (rule-based audiences exist) with rollout %, scheduled overrides.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Surveys: questions with metadata key, field types (short/long text, number, date, checkbox, select, multi-select), requ…* — rg -i 'survey' = 0
  - ❌ Metaplay: *Experiment variants defined as config variant columns, visible in config detail diff* — no variants
  - ❌ Metaplay: *A/B experiments as game-config variants: `/Variant` rows or `/:` variant columns, multiple variants per row, `PlayerExp…* — parity.md: experiments delegated to Zanshin; no config patching per cohort
  - ❌ UGS LiveOps: *`configAssignmentHash` (pin a player's assigned config, TTL 56 h)* — No assignment snapshot; sketch: hash of (config version, audience membership) returned in `GET /v1/configs` and accepted by store/rpc calls
  - 🟡 Beamable: *Realm launch message (maintenance / kill-switch)* — Login queue + flags; no dedicated launch-message endpoint
  - 🟡 LootLocker: *Game Settings: Game ID / Title ID / Environment ID, name, Game Status kill-switch (reject all auth), Danger Zone delete…* — Login queue/maintenance via `[auth]` config + `POST /v1/admin/auth/queue`; projects/environments exist; no single "game off" toggle via API (rg 'maintenance' = 0)
  - 🟡 UGS LiveOps: *Config caching / `configAssignmentHash` (stable per-player config incl. Game Overrides variants)* — Store listings resolve per audience server-side; no per-player config snapshot hash a client can pin

#### P0-53. Live events (scheduled server-owned events with phases, templates, local time, targeting)  
*area:* progression/LiveOps · *musubi:* 🟡 partial (2h/10p/9m over 21 rows)
- **Competitors with it (4):** Metaplay, Hiro/Satori, Nakama, UGS LiveOps
- **Why it matters:** Content cadence drives LiveOps.
- **Implementation sketch (musubi terms):** Exists: `/v1/liveops/live-events` via Zanshin. Gap: native event objects with phases, templates, local-time scheduling, targeting (musubi-economy/liveops.rs or Zanshin parity).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Metaplay: *Two event systems: config-driven Activables (all events declared in shared config, state resolved locally) and LiveOps…* — musubi has no in-game event framework; `/v1/liveops/*` = Zanshin flag/event integration, seasons/battle pass are separate (README progression row)
  - ❌ Metaplay: *Config-driven event schedule fields: `Schedule.TimeMode` Utc/Local, Start Date/Time, Preview, Duration, EndingSoon, Rev…* — seasons have start/end only; no preview/ending-soon/review phases, no player-local time
  - 🟡 Metaplay: *LiveOps Events: create/edit from dashboard without config or client update; event templates/presets from server-only co…* — server API: live events are DB rows editable at runtime (`PUT /v1/server/live_events/{id}`), templates via `/v1/server/live_event_templates` + `from_template`; no dashboard yet
  - ❌ Hiro/Satori: *Satori LiveOps platform: console + client SDK + console API* — musubi has no LiveOps console/UI; flags & live events only by proxying an external Zanshin deployment (`[zanshin]` config, `GET /v1/liveops/flags\|live-events`)
  - ❌ Hiro/Satori: *Custom metrics (Count/Sum/Avg… types, High/Low order) reused as goal/monitor metrics; Explore Metrics chart with multip…* — none
  - ❌ Hiro/Satori: *Client API: API-key authenticate -> session token + refresh; flags/overrides with API key; events; live events (list, j…* — musubi has no client SDKs (README: "client SDKs ... not written yet"; parity.md lists Unreal scaffold)
  - ✅ Hiro/Satori: *Category labels across flags/live events/experiments/messages with global management* — `labels[]` on flags, experiments, live events (and `tags[]` on news); `?label=` filters on the server lists and on `GET /v1/server/liveops/timeline`
  - ✅ Hiro/Satori: *Explicit participation mode (2.1.2), peek past/future live events (2.1.3), participation duration (2.2.1), labels filtering* — native live events: `participation: explicit` + `participation_secs`/`max_participants` with `POST /v1/liveops/live-events/{id}/join|leave` (own window per cycle), `GET /v1/liveops/live-events?include_past&include_future&labels=`, `labels[]` on events and templates (`/v1/server/live_event_templates`, `POST /v1/server/live_events/from_template`), `GET …/{id}/participants`
  - 🟡 Metaplay: *Audience targeting UI shared by Broadcasts, Push campaigns, Experiments, LiveOps Events: whole base / segments / list o…* — mail broadcast + push + store accept audience ids; no explicit player-id lists in same UI, no size estimate
  - 🟡 Hiro/Satori: *Personalizer pipeline (JSON -> StoragePersonalizer -> SatoriPersonalizer -> custom)* — musubi: content bundles (`GET/POST /v1/admin/content` with diff/history/rollback) give server-wide definition overrides without redeploy; store level gates/audiences for mail; no per-player/segment definition overlay or…
  - 🟡 Hiro/Satori: *Feature flags (remote config)* — musubi: `GET /v1/liveops/flags?names=` proxies Zanshin flags (30 s cache, invalidated by `POST /v1/server/liveops/changed`) and typed `configs` documents (`GET /v1/configs/{name}`, admin PUT) for global remote config; n…
  - 🟡 Hiro/Satori: *Live events* — musubi: `GET /v1/liveops/live-events` proxied from Zanshin; natively: seasons (0017), store availability windows, tournaments with schedules, schedules.rs cron jobs for modules; no enrollment model, participation durati…
  - 🟡 Hiro/Satori: *Attach player messages to live events (before/on/after start/end offsets)* — musubi push templates with schedule + tz (push.rs) and mail audiences; no coupling to an event timeline
  - 🟡 Hiro/Satori: *Journeys: visual node-based per-player lifecycle flows* — musubi workflows (durable, `/v1/server/workflows`, steps/timers/retries) are a server-programmable engine, not an operator-facing per-player lifecycle builder with messaging/flag actions
  - 🟡 Hiro/Satori: *Console API (server key Basic auth) mirrors console: flags, audiences, experiments, live events, messages, identities,…* — musubi admin HTTP API (`/v1/admin/*`, 93 routes) with key roles + audit log covers its own domain; no LiveOps objects
  - 🟡 Hiro/Satori: *Data management: import/export configuration between Satori instances (select flags/experiments/live events/schedules,…* — musubi content bundles export/import with dry-run/prune/history/rollback + audit log (0026); no revert-from-audit, no labels
  - 🟡 Hiro/Satori: *Win-back campaign recipe: audience of lapsed players, offer live event, scheduled message, measure with metric* — musubi mail audience `inactive_days` + reward attachments cover the delivery half; no measurement
  - 🟡 UGS LiveOps: *Payload versioning* — Event envelope has `type` + `version` fields; no per-trigger version selection
- **Already covered rows (2):** Nakama: Satori integration: `GetSatori()` client (authenticate, identity dele…; UGS LiveOps: Supported UGS events

#### P0-54. Standalone UDP/DTLS relay for third-party netcode (Mirror/FishNet/NGO/UTP/Unreal transports) with join codes  
*area:* realtime/matches · *musubi:* 🟡 partial (0h/2p/1m over 5 rows)
- **Competitors with it (4):** AccelByte, Beamable, Edgegap, UGS Multiplayer
- **Why it matters:** Lets studios keep their netcode and drop the DS.
- **Implementation sketch (musubi terms):** Add `POST /v1/relay/allocate` (join code, max peers, region) on musubi-edge UDP; adapter packages for Mirror/FishNet/NGO/UE transports.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *Relay transports for Unity netcodes* — No Mirror/FishNet/NGO transport
  - 🟡 Beamable: *Photon Fusion / third-party netcode* — Match id + lobby start payload usable by any netcode; no documented Photon recipe
  - 🟡 UGS Multiplayer: *Relay allocation linkage per player* — Match presence events track socket membership; not linked into lobby membership state.

#### P0-55. Scoped / down-scoped tokens, publishable keys, per-action client policies  
*area:* auth · *musubi:* 🟡 partial (0h/2p/1m over 3 rows)
- **Competitors with it (3):** Colyseus, EOS, UGS LiveOps
- **Why it matters:** Lets untrusted clients and third parties hold least-privilege credentials.
- **Implementation sketch (musubi terms):** Add `scopes[]` claim to musubi-auth tokens + per-route scope check in musubi-edge; mint via `POST /v1/auth/token/scope` and server keys with resource scopes.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *Resources & CRUD: every drizzle table → paginated list with search/filter/sort, detail page with related-row tabs + act…* — no table browser/CRUD UI; storage admin via `GET/PUT/DELETE /v1/admin/storage/...` and CLI export/import
  - 🟡 EOS: *Custom Invites Interface: game-defined invite payload delivered via EOS to Social Overlay / Epic Games Launcher / Store…* — musubi notifications (persistent, typed payload) + PartyInvite can carry arbitrary payload; no launcher/store delivery or overlay UI
  - 🟡 UGS LiveOps: *Restricted tokens (caller-controlled scoped session tokens)* — Server keys have roles/limits (`POST /v1/admin/keys`), module RPCs can mint custom sessions; no player-scoped down-scoped token API

#### P0-56. Game server SDK (ready/health/allocate/shutdown/label/connect/players, watch stream, native libs)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (0h/3p/0m over 4 rows)
- **Competitors with it (3):** Agones/OM, GameLift, PlayFab
- **Why it matters:** Every DS integration starts here.
- **Implementation sketch (musubi terms):** Exists: HTTP SDK routes (`/v1/gameserver/*`, long-poll watch). Gap: native libs (UE/Unity/C++/C#/Go/Rust/Node) + push WatchGameServer stream (WS/gRPC).
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Agones/OM: *Out-of-cluster dev server: local SDK server with kubeconfig + dev-address GameServer, allocatable from the real cluster…* — `webhook` provider lets an external orchestrator register servers (README:Webhook fleet provider) — needs you to build the orchestrator; no documented IDE flow
  - 🟡 PlayFab: *Game Server SDK (GSDK): C#, C++, Java, Unity, Unreal (plugin), Go/Node? — Start, ReadyForPlayers, heartbeats, callbacks…* — README gameserver HTTP SDK (ready/health/allocate/shutdown/label/connect/watch long-poll/players) + `musubi-sim-sdk`; no language-specific GSDK libraries or Unity/Unreal plugin, no local agent emulator (musubi fleet loc…
  - 🟡 PlayFab: *GSDK callbacks & config: Start, ReadyForPlayers (blocks until Active), GetGameServerConnectionInfo (ports), GetConfigSe…* — README gameserver HTTP SDK (`/v1/gameserver/*`: ready, health, players, labels, allocation watch, shutdown) + sim-sdk; no language libraries/engine plugins, no maintenance callback

#### P0-57. Offers & placements (offer groups, priority, cooldown, precursor chains, per-player limits, purchase intents)  
*area:* economy/IAP · *musubi:* 🟡 partial (1h/2p/1m over 5 rows)
- **Competitors with it (3):** Metaplay, Hiro/Satori, Steamworks
- **Why it matters:** Mobile F2P revenue depends on targeted offers.
- **Implementation sketch (musubi terms):** Add `offer_groups`/`placements` in musubi-economy with activation limits, cooldown, precursor chains, per-player force-enable and seen/purchased stats.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Metaplay: *Offer lifecycle tooling: force-enable per player from dashboard (bypasses `Enabled`), Unity offers debug tool, ineligib…* — no per-player force-enable or seen/purchase stats per offer
  - 🟡 Metaplay: *Dynamic-content purchases (`HasDynamicContent`, offers), purchase analytics `PlayerInAppPurchased`, duplicate transacti…* — transaction dedupe via receipt id; no dynamic-content binding of offer→SKU
  - 🟡 Hiro/Satori: *One-time / segment-specific offers via Personalizer* — musubi: store `max_per_player`/purchase limits, level gates, audiences for mail; no per-segment price/reward personalization pipeline, no A/B
- **Already covered rows (1):** Steamworks: Trading & Community Market

#### P0-58. Matchmaking observability (per-ticket X-Ray/log, rule evaluation metrics, stored results)  
*area:* matchmaking · *musubi:* 🟡 partial (0h/2p/0m over 2 rows)
- **Competitors with it (2):** Edgegap, UGS Multiplayer
- **Why it matters:** Debugging bad matches is the #1 ops ask.
- **Implementation sketch (musubi terms):** Exists: stats p50/p95, simulate. Add `matchmaking_ticket_log` (sampled per-ticket decision trace) + `GET /v1/server/matchmaker/tickets/{id}/trace`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Edgegap: *Matchmaking Analytics* — Prometheus histograms + p50/p95/p99 wait per queue×region as JSON; no dashboard
  - 🟡 UGS Multiplayer: *Stored matchmaking results* — Match results/journals stored (`match_results` migration) and matchmaker proposal delivered to the allocated server in payload; no stable "results by matchId" admin endpoint (`rg -i 'matchmaking.?result'` → 0).

#### P0-59. Sanctions service (typed actions, durations, pending/approve, appeals, client-queryable, sync feed)  
*area:* moderation/anti-cheat · *musubi:* 🟡 partial (0h/4p/1m over 5 rows)
- **Competitors with it (2):** EOS, UGS Multiplayer
- **Why it matters:** Structured enforcement with appeals is a cert/legal need.
- **Implementation sketch (musubi terms):** Add `sanctions` (type, duration, pending/approved, tags, justification) + `GET /v1/account/sanctions` + appeals workflow via workflows; bans become one sanction type.
- **What the reports found weaker/missing (merged rows):**
  - ❌ EOS: *Sanction appeals: player-created appeal via SDK (reason enum), Epic-hosted appeal form by reference id, Web API; portal…* — no appeal workflow in musubi
  - 🟡 EOS: *Developer Portal Player Reports page: 30-day bar graph (daily/hourly), CSV download, filter by reported player/time, as…* — moderator queue API (/v1/admin/reports: list/assign/resolve → ban) exists; no UI/graphs/CSV
  - 🟡 EOS: *Sanctions Interface: sanction = action string (custom a-zA-Z, built-ins RESTRICT_GAME_ACCESS, RESTRICT_MATCHMAKING), du…* — musubi bans: timed/permanent, shadow, reason/message, kick (/v1/admin/accounts/{id}/ban) + per-bracket feature closure; no typed multi-action sanction catalogue (e.g. comms-only ban) queryable by client, no pending/appr…
  - 🟡 EOS: *Portal: edit/activate/close sanctions, filter by status/tags/player, bar graph by action, CSV export; roles* — admin ban API + audit log; no UI
  - 🟡 UGS Multiplayer: *Configurable sanctions & honors* — Bans (temp/perm), shadow bans, mutes exist; no customizable sanction/honor catalog or positive rewards for behaviour.

#### P0-60. P2P / NAT traversal (ICE/STUN/TURN) with relay fallback  
*area:* realtime/matches · *musubi:* ❌ missing (1h/1p/1m over 3 rows)
- **Competitors with it (2):** Photon, Steamworks
- **Why it matters:** Listen-server/P2P games avoid DS cost.
- **Implementation sketch (musubi terms):** Missing. Add ICE candidate exchange over realtime channel + TURN-like relay tier on musubi-edge (`/v1/rtc/*` exists for WebRTC offer only).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Photon: *Turn-based/async games* — no turn-based match type with persisted turn state/notifications; would compose storage + mail + push manually
  - 🟡 Steamworks: *Legacy P2P API* — covered by relayed matches via server; no direct NAT traversal between peers
- **Already covered rows (1):** Photon: End-to-end connection encryption (2.0)

#### P0-61. Server-side SDKs for game servers (Unreal DS, Unity server, C++, C#, Go, Rust, Node, Python)  
*area:* SDKs · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** UGS Multiplayer
- **Why it matters:** DS and backend integrations.
- **Implementation sketch (musubi terms):** Generate from openapi: Go, Rust, C#, TS server clients with `X-Musubi-Server-Key`; DS SDK helper libs for UE/Unity.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 UGS Multiplayer: *Dedicated-server-side SDK (`MultiplayerServerService`, `IServerSession`, `ServerAuthenticationService.SignInWithService…* — HTTP game-server SDK (`/v1/gameserver/*`, server key) + WASM server modules; no C# server library.

#### P0-62. Content/config editors (economy, leaderboards, achievements, campaigns, remote config, segments) with publish workflow  
*area:* console/admin · *musubi:* ❌ missing (0h/1p/1m over 2 rows)
- **Competitors with it (1):** Beamable
- **Why it matters:** LiveOps without engineers.
- **Implementation sketch (musubi terms):** Console forms over existing server routes + draft/publish stage.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Beamable: *Realm Config editor w/ per-key saves & external-change warning* — Config API + audit; no UI

#### P0-63. Live inspectors (rooms/matches/lobbies/worlds, sessions, send-as-client, force disconnect)  
*area:* console/admin · *musubi:* ❌ missing (0h/1p/2m over 3 rows)
- **Competitors with it (1):** Colyseus
- **Why it matters:** Debug live gameplay.
- **Implementation sketch (musubi terms):** Console pages over `/v1/admin/cluster/matches`, `/v1/admin/worlds/*`, `/v1/admin/accounts/{id}/sessions`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *`@colyseus/admin` (beta) browser ops console: user management (browse, ban, revoke sessions), CRUD over drizzle tables,…* — no admin UI at all (README: "admin console not written yet"); all ops are HTTP `/v1/admin/*` + `musubi` CLI
  - ❌ Colyseus: *Monitoring Panel `@colyseus/monitor` (`/monitor`): list rooms (custom columns), force dispose, inspect room state, send…* — no web UI; equivalents are `GET /v1/admin/cluster/matches`, `GET /v1/server/matches/{id}`, kick/terminate endpoints
  - 🟡 Colyseus: *Live room inspector: list rooms (id, name, clients, max, locked/private, uptime, process), detail (clients w/ sessionId…* — `GET /v1/admin/cluster/matches\|worlds\|nodes`, `GET /v1/server/matches/{id}` (players, label, snapshot), kick/ban/terminate via API & CLI, audit log; no UI, no live state-tree browsing

### 3.2 P1 — 65 items

#### P1-1. Google / Google Play Games Services login  
*area:* auth · *musubi:* 🟡 partial (3h/3p/0m over 6 rows)
- **Competitors with it (5):** Beamable, brainCloud, LootLocker, Pragma, UGS LiveOps
- **Why it matters:** Android default identity.
- **Implementation sketch (musubi terms):** Exists: `/v1/auth/google`; add Play Games server-auth-code exchange in musubi-auth/google.rs.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Beamable: *Google Play Games Services sign-in (Android only)* — Google id-token auth only; no GPGS server-auth-code exchange
  - 🟡 LootLocker: *Google Play Games v2 auth (server auth code exchange with client id/secret/app id)* — Google id-token login exists (`/v1/auth/google`); no Play Games Services v2 auth-code exchange / Play Games player id mapping (rg 'play games' = 0)
  - 🟡 UGS LiveOps: *Google Play Games* — Google id-token login exists; no dedicated Play Games Services (server auth code) flow — `rg "play games"` empty
- **Already covered rows (3):** Beamable: Google sign-in (OAuth id token); brainCloud: Google Play Games & Google OpenID; Pragma: Google identity provider

#### P1-2. Deployment / server object model (lifecycle states, status API, tags, metadata, FQDN, TLS termination, context endpoint)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (12h/13p/1m over 27 rows)
- **Competitors with it (5):** Edgegap, GameLift, Hathora, Photon, PlayFab
- **Why it matters:** Operators and game servers need a complete, inspectable server object.
- **Implementation sketch (musubi terms):** Extend `gameservers` rows: `tags`, `metadata`, `state` history, `fqdn`/TLS via edge, `GET /v1/gameserver` context already exists; add `PATCH` for tags.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *Deployment FQDN* — Servers addressed by node IP:port; no per-server DNS name
  - 🟡 Edgegap: *Deployment timeout* — Reconciler reaps servers not healthy within `HEALTH_TIMEOUT` (30 s, `musubi-fleet/src/lib.rs`); not configurable per fleet
  - 🟡 Edgegap: *Managed TLS upgrade of ports (WSS/HTTPS)* — `musubi-edge` terminates TLS for musubi's own ingress (README crate table), not for third-party dedicated-server ports
  - 🟡 Edgegap: *Deployment constraints / filters on version* — Fleet is bound to one `region` string; no geographic constraint language
  - 🟡 Edgegap: *Version tags & app lock while in use* — Active build cannot be retired (README 5.9); no tags on builds
  - 🟡 Edgegap: *Deployment tags* — `GameServer.labels` JSON + `POST /v1/gameserver/label`; filterable only via `GET /v1/matches?label=` for cluster matches
  - 🟡 Edgegap: *Deployment lifecycle logs* — Audit log + `GameServer` timestamps; no per-server event timeline
  - 🟡 Edgegap: *Host lifecycle states* — Cluster nodes have heartbeat/drain states (`GET /v1/server/cluster/nodes`); SSH hosts have capacity but no maintenance/recycle states
  - 🟡 GameLift: *Multiple server processes per instance* — one process per server slot; `FleetSpec.command`, `port_start/port_end`; no activation timeout / max concurrent activations knobs
  - 🟡 GameLift: *Fleet TLS certificate* — musubi edge terminates TLS / QUIC / WebTransport centrally; no per-fleet server certificate issuance to game servers
  - 🟡 GameLift: *Compute object* — `GameServer{id, fleet_id, address, port, state, labels, build}` — no compute-level (host) object separate from server
  - 🟡 GameLift: *Instances listing* — servers only; hosts belong to the provider
  - 🟡 GameLift: *Deployment validation* — canary % + manual promote/rollback via `promote`
  - 🟡 Hathora: *Managed TLS termination for `tls` transport* — musubi edge terminates TLS for its own WS/HTTP (`musubi-edge`); no TLS termination in front of arbitrary dedicated-server ports
- **Already covered rows (12):** Edgegap: Application as top-level container of versions; Edgegap: Command / arguments override; Edgegap: Lifecycle states; Edgegap: Status API; Edgegap: Self-stop from inside container; Edgegap: Deployment context endpoint for the container; Edgegap: Deployment retries (self-healing fleets / matchmaker `max_deployment_…; GameLift: Runtime configuration hot update; Hathora: Deployment tags; Photon: TLS certificate setup; PlayFab: Server lifecycle: states Initializing→StandingBy→Active→Terminated, n…; PlayFab: Server lifecycle policies: max VM lifetime 10 days (≥23 h), process c…

#### P1-3. Fleet capacity controls (min available/max per region, availability buffer, manual capacity, utilization charts, density)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (7h/6p/1m over 15 rows)
- **Competitors with it (5):** Edgegap, GameLift, Hathora, PlayFab, UGS Multiplayer
- **Why it matters:** Capacity planning basics.
- **Implementation sketch (musubi terms):** Exists: buffer/min/max per fleet. Add per-region min/max + utilization time series.
- **What the reports found weaker/missing (merged rows):**
  - ❌ GameLift: *Console fleet metrics page* — no UI
  - 🟡 Edgegap: *Utilization charts* — `GET /v1/server/fleets/{id}/usage` vCPU-hours per build/region/provider (JSON, no UI)
  - 🟡 GameLift: *Manual capacity & limits per location* — per-fleet min/max/buffer; region = separate fleet in a pool, no per-location sub-capacity
  - 🟡 GameLift: *Container fleet scaling formulas* — k8s HPA disabled by default in Helm; musubi scales servers not nodes
  - 🟡 Hathora: *Fleet metrics* — `GET …/fleets/{id}/usage?from&to` vCPU-hours per build; `GET …/fleets/{id}/demand`; no provisioned-vs-utilized node capacity series
  - 🟡 UGS Multiplayer: *Server density / server slots* — `FleetSpec` vcpu/mem per server; packing is delegated to the provider (k8s requests/limits, firecracker VM size); no cross-machine density planner.
  - 🟡 UGS Multiplayer: *Fleet hierarchy (account service → fleet → fleet region → location → machine → server slot)* — project → fleet(region) → server; no machine/location layers.
- **Already covered rows (7):** GameLift: Fleet utilization & capacity; PlayFab: Multiplayer Servers 2.0 (Thunderhead): Azure-hosted dedicated game se…; PlayFab: Standby scaling: Default (scale to max, down on session end), Schedul…; PlayFab: Multiplayer API: CreateBuildWithManagedContainer / CreateBuildWithCus…; UGS Multiplayer: Fleet regions online/offline (draining); UGS Multiplayer: Scaling: min available / max servers per region; UGS Multiplayer: Availability buffer

#### P1-4. State sync / schema codec (field-level change tracking, per-field delivery modifiers, quantization, callbacks)  
*area:* realtime/matches · *musubi:* 🟡 partial (2h/4p/0m over 7 rows)
- **Competitors with it (5):** Colyseus, Photon, PlayFab, Pragma, UGS Multiplayer
- **Why it matters:** Developer ergonomics for replicated state.
- **Implementation sketch (musubi terms):** Exists: IDL types + RFC 6902 deltas (`match_state_delta`). Gap: per-field delivery modifiers (unreliable/patch-only/stream), quantized floats, generated client callbacks.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Colyseus: *Quantized floats `t.quantized({min,max,bits 8\|16\|32,mode clamp\|wrap})`, `t.angle()` sugar — identical lossy value on…* — no quantization in IDL/match state deltas (`rg -i quantiz` → none) *(verdict changed, §5.3)*
  - 🟡 Colyseus: *Field modifiers: `.default()`, `.view(tag)`, `.deprecated(throws?)`, `.optional()`, `.noSync()`, `.unreliable()`, `.pat…* — `sync_exclude` paths (private/never-synced) and per-channel semantics exist; no per-field unreliable / patch-only / full-state-only delivery modes
  - 🟡 Photon: *[Networked] properties* — IDL typed match state → match_state/match_state_delta (RFC6902 patches) (`musubi-idl`, `musubi idl from-colyseus`); no engine-side weaving
  - 🟡 Photon: *Input & Commands* — match_input reliable/unreliable messages; no delta compression of input stream specifically
- **Already covered rows (2):** PlayFab: Real-time notifications (RTN): SignalR hub (JSON/MessagePack), StartO…; Pragma: Game instance tasks: CreateGameInstanceV1 / JoinGameInstanceV1 / Leav…

#### P1-5. Console SDKs (PlayStation, Xbox GDK, Nintendo Switch) and certification notes  
*area:* SDKs · *musubi:* ❌ missing (0h/2p/3m over 8 rows)
- **Competitors with it (4):** EOS, Meshing vendors, Photon, PlayFab
- **Why it matters:** Console shipping is gated on this.
- **Implementation sketch (musubi terms):** C++ core SDK with platform TLS/socket hooks + cert checklists per platform (NDA docs out of band).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Meshing vendors: *Certifications* — none published
  - ❌ Photon: *Console support* — Xbox XSTS/PSN/Nintendo auth exists but no console SDK/certification docs
  - ❌ PlayFab: *Platform guides: Android, iOS, macOS, Linux, Windows, GDK (Xbox/PC), PlayStation, Nintendo Switch, services (server) —…* — No platform integration guides beyond Unreal plugin notes (README: Unreal-first)
  - 🟡 EOS: *Fully supported platforms: Windows (10+, incl. Arm), macOS (10.12+/Apple Silicon), Linux (glibc 2.17+, ARM64), iOS 12+,…* — musubi server is platform-agnostic HTTP/WS; platform auth for Steam/PSN/Xbox/Nintendo exists but no client libs/console certifications
  - 🟡 EOS: *Console SDKs: PS4/PS5, Xbox GDK (versions pinned per release), Nintendo Switch/Switch 2 (SDK 22.2.5) — overlay, voice,…* — PSN/Xbox/Nintendo token auth + entitlements s2s exist server-side; no console client libraries or certification guidance

#### P1-6. Editor tooling (deployment window, in-editor config authoring, content sync, inspector, playground)  
*area:* SDKs · *musubi:* 🟡 partial (2h/2p/0m over 5 rows)
- **Competitors with it (4):** Colyseus, LootLocker, Metaplay, UGS LiveOps
- **Why it matters:** Designers work in the engine.
- **Implementation sketch (musubi terms):** After SDKs: Unity Editor window for `musubi deploy`, content bundles diff/publish, live room inspector.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Colyseus: *Playground `@colyseus/playground` (`/playground`): create/join/joinById rooms in browser, auth auto-detected, simulate…* — `GET /openapi.json` → Swagger/any REST client; request recording/replay in dev tools (`0092_dev_tools`); no built-in room/WS playground UI
  - 🟡 UGS LiveOps: *Authoring: Editor window, dashboard, CLI (`.rc` files), REST Admin API, config-as-code* — `PUT /v1/server/configs/{name}`, `musubi deploy`, content bundles with diff/rollback; no editor window
- **Already covered rows (2):** Colyseus: 0.16 → 0.17: `getAvailableRooms()` removed from SDKs (use LobbyRoom/c…; LootLocker: Three API surfaces: Game API (client), Server API (trusted game serve…

#### P1-7. Account deletion / soft delete / restore / dormant policy  
*area:* accounts · *musubi:* 🟡 partial (1h/2p/1m over 4 rows)
- **Competitors with it (4):** Beamable, brainCloud, EOS, LootLocker
- **Why it matters:** GDPR and store policies.
- **Implementation sketch (musubi terms):** Exists: `DELETE /v1/account`, `/v1/admin/accounts/{id}/restore`. Gap: dormant-account auto-archive policy with email notice (`[retention] dormant_days`).
- **What the reports found weaker/missing (merged rows):**
  - ❌ brainCloud: *Archive / auto-delete dormant users* — retention lease cleans events/sessions only (`[retention]`), no dormant-account archive/delete policy
  - 🟡 EOS: *PDS limits: 200 MB max file (auto-delete >400 MB), 400 MB total per user, 1000 files per user, 1000 read/write req/min;…* — musubi quotas 10k objects/100 MiB per user (configurable), UGC blob ≤16 MiB — smaller per-file ceiling but configurable
  - 🟡 LootLocker: *Asset Manager console: list/grid, column picker, stacked filters (created date, filter key, filter value), batch edit f…* — Item CRUD + tags/kind filters via API, content export/import; no console UI, no player favourite-assets API (favourites exist only for friends)
- **Already covered rows (1):** Beamable: Account deletion sample (GDPR/App Store requirement)

#### P1-8. Epic Games (EOS) login  
*area:* auth · *musubi:* 🟡 partial (3h/5p/1m over 10 rows)
- **Competitors with it (4):** EOS, LootLocker, Photon, Pragma
- **Why it matters:** Epic store titles and Unreal projects expect it.
- **Implementation sketch (musubi terms):** Add `POST /v1/auth/epic` verifier (EOS JWKS) in musubi-auth; OIDC provider config covers it only generically today.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Pragma: *OpenID Connect provider: Epic Connect* — musubi is an OIDC *relying party* only; it does not issue OIDC identities to third parties (`musubi-server/src/http/sso.rs` is inbound).
  - 🟡 EOS: *Multi-user local sign-in (several Epic accounts on one device/game instance)* — musubi sessions are per-connection; multiple sockets possible but no first-class local multi-user API
  - 🟡 EOS: *Identity model: Epic Games account (EAS) vs EOS Organization User ID / Product User ID (PUID); external identity linked…* — musubi has single account id per project; no org-level vs product-level user id split; multi-project via [[projects]] with separate accounts
  - 🟡 EOS: *Player Search page: lookup by PUID / Epic account id / external account id; Player Profile tab (achievements, stats, PD…* — admin API: /v1/admin/accounts search, identity lookup, /v1/account/export (+admin export/restore), unlink, purge — API only, no UI, no rate-limited export bundle via email
  - 🟡 Photon: *Epic EOS auth* — rg eos=0 (epic hits are unrelated) *(verdict changed, §5.3)*
  - 🟡 Pragma: *Easy Anti-Cheat (EOS) integration guide* — `musubi-anticheat` signals + 2 routes (`/v1/anticheat/*`) for reports/heuristics; no EAC/EOS token flow documented.
- **Already covered rows (3):** EOS: Account deletion of user data per application (user-initiated from Ep…; LootLocker: Epic Games auth (Epic Sign In + Epic Connect identity tokens); Pragma: Epic identity provider

#### P1-9. Magic link / passwordless email login  
*area:* auth · *musubi:* 🟡 partial (2h/2p/0m over 4 rows)
- **Competitors with it (4):** brainCloud, LootLocker, SpacetimeDB, UGS LiveOps
- **Why it matters:** Reduces drop-off; common in web games.
- **Implementation sketch (musubi terms):** Exists: `/v1/auth/email/magic*`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *Self-service account deletion & branding* — `DELETE /v1/account` + `/v1/admin/accounts/{id}/export` exist; no hosted end-user web pages / branding
  - 🟡 LootLocker: *Magic Link modes: Disabled / Sign-In only / Sign-Up & Sign-In / Magic Link Only; link valid 1h; applies to hosted pages…* — `POST /v1/auth/email/magic` exists; no mode switch to disable passwords or sign-in-only restriction
- **Already covered rows (2):** SpacetimeDB: Email magic link; UGS LiveOps: MFA (TOTP), passkeys/WebAuthn, recovery codes, magic links

#### P1-10. Chat moderation (filters, mute, slow mode, reports, automod webhook, translation)  
*area:* chat · *musubi:* 🟡 partial (2h/3p/0m over 5 rows)
- **Competitors with it (4):** brainCloud, Colyseus, Nakama, UGS Multiplayer
- **Why it matters:** T&S requirement.
- **Implementation sketch (musubi terms):** Exists: filters, mute/shadow-ban, automod/translate webhooks. Gap: built-in ML classifier provider, assert-vs-mask modes.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *Profanity filter (WebPurify)* — chat filters + external automod webhook (README 5.18); no standalone text-check API for arbitrary strings outside chat
  - 🟡 Colyseus: *Curated 3rd-party Node packages: seedrandom, miniplex ECS, quadtree-ts, three-mesh-bvh, bounce/crashcat 3D physics, Pla…* — WASM modules can bundle any Rust/AssemblyScript crate (no_std ECS/physics compile to wasm32); built-in chat word filter (`musubi-chat/src/moderation.rs`, `/v1/admin/chat/filters`); no curated catalog
  - 🟡 UGS Multiplayer: *Safe Text batch filter API* — Word-filter + automod webhook only.
- **Already covered rows (2):** brainCloud: Messages; Nakama: Moderation: none built in (profanity filter/mute/report must be custo…

#### P1-11. Node-level autoscaling / cloud burst / capacity provisioning (bare-metal first, spot)  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (0h/2p/4m over 7 rows)
- **Competitors with it (4):** AccelByte, Agones/OM, GameLift, Hathora
- **Why it matters:** Hybrid cost model (Hathora/Edgegap).
- **Implementation sketch (musubi terms):** Add capacity-provider abstraction in musubi-fleet (`provision/deprovision node`) with Hetzner/OVH/AWS drivers + spot termination notice relay.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *Windows game servers (alpha) on Windows node pools* — k8s provider assumes Linux images; ssh provider shells `cd … && env …` (README:ssh provider) — no Windows host support
  - ❌ GameLift: *Spot instances* — `rg -i 'spot' or 'preempt' crates/musubi-fleet` → none
  - ❌ Hathora: *Hosting class visibility* — fleets have `pool` names but no hardware class attribute
  - 🟡 GameLift: *Fleet design guidance* — README fleet section; no sizing guidance
  - 🟡 GameLift: *Production hosting checklist* — README deploy docs, Helm PDB/replicas

#### P1-12. Server states: Reserved/Ready/Allocated/Shutdown, idle TTL reaper, max duration  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (2h/1p/2m over 5 rows)
- **Competitors with it (4):** Edgegap, Meshing vendors, Metaplay, Rivet
- **Why it matters:** Prevents orphaned/zombie servers.
- **Implementation sketch (musubi terms):** Add `reserved_until` + `idle_ttl_secs`/`max_duration_secs` in FleetSpec; reaper on the fleet lease (musubi-fleet).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *Max duration per deployment* — No per-server TTL / max lifetime in fleet model (`rg max_duration\|max_lifetime` → none); servers live until shutdown/unhealthy
  - ❌ Edgegap: *Relay max duration failsafe* — No relayed-match TTL
  - 🟡 Metaplay: *Akka.NET actor-based game server: persisted vs ephemeral entities, `EntityAsk<>`, PubSub subscriptions, `AutoShutdownPo…* — musubi-actor crate: WASM actors with mailbox, timers, supervision, snapshots, cross-node delivery (README actors row); no C# in-process actor model, logic runs in WASM sandbox
- **Already covered rows (2):** Meshing vendors: Auto-shutdown; Rivet: Ports: 6420 API/dashboard/WS, 6421 peer/health; LB idle timeout ≥3,60…

#### P1-13. IAP providers beyond the basics (Amazon, Microsoft Store, Facebook Instant, Meta Horizon, Epic, PSN/Xbox entitlements, Discord, Unity IAP)  
*area:* economy/IAP · *musubi:* ❌ missing (1h/2p/2m over 6 rows)
- **Competitors with it (4):** AccelByte, Beamable, Nakama, Pragma
- **Why it matters:** Each platform launch needs its store verified.
- **Implementation sketch (musubi terms):** Exists: entitlements sync (Steam/Epic/PSN/Xbox?). Add verifiers in musubi-economy/iap/*.rs per provider; Twitch Drops as entitlement provider.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Nakama: *Facebook Instant Games purchase validation (3.19)* — rg -i "instant" → none
  - ❌ Pragma: *Twitch details* — Twitch only as OIDC identity (`rg -i twitch` → auth only).
  - 🟡 AccelByte: *IAP (63) + IAP Subscription (17) + IAP Notification: configs per platform, sync endpoints, order history, mock, Steam a…* — platform IAP/subscriptions have; native recurring billing subscription (real money) missing
  - 🟡 Nakama: *Unity IAP receipt format accepted (wrapped Apple/Google payloads)* — raw provider receipts only (rg -i "unity iap" → none)
- **Already covered rows (1):** Pragma: Epic details

#### P1-14. Cohort / event leaderboards with tiers, promotion/demotion, re-rolls  
*area:* leaderboards/tournaments · *musubi:* 🟡 partial (2h/5p/0m over 8 rows)
- **Competitors with it (4):** brainCloud, Metaplay, Hiro/Satori, UGS LiveOps
- **Why it matters:** Mobile competitive meta (Hiro/Metaplay).
- **Implementation sketch (musubi terms):** Exists: `bucket_size` + leagues `tiers`. Gap: idle demotion, score-target races, re-rolls, per-tier overrides, pacers.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *Divisions (division sets)* — leaderboard buckets/leagues exist (README 5.x) but no tournament-division config with auto-instancing + per-instance prizes
  - 🟡 Metaplay: *Season cycle options: `SeasonCycleStartDate`, `SeasonCycleRecurrence`, `SeasonCycleRestPeriod`, `SeasonCycleEndingSoonP…* — promote/demote by percentage at reset only; no rest/ending-soon phases, no custom placement hooks beyond WASM
  - 🟡 Metaplay: *Dashboard: league overview, seasons, division inspector, player's league card, manual season advance, league debug cont…* — `POST /v1/server/leaderboards/{id}/rollover` + admin reads only, no UI
  - 🟡 Hiro/Satori: *Tiers with promotion/demotion* — musubi leagues (divisions with promote/demote counts per window, rank rewards) cover promotion/demotion by rank; missing percentage zones, idle demotion, decay per phase, per-tier overrides, live tier_delta preview
  - 🟡 Hiro/Satori: *Team event leaderboards (cohorts, tiers, score-based)* — group challenges / clan wars (0017? group_challenges) pit groups by score; no tiered cohorts
- **Already covered rows (2):** Metaplay: Leagues = sharded season-based competition: ranks (e.g. Bronze/Silver…; UGS LiveOps: Buckets (cohorts of N players)

#### P1-15. Player statistics (typed stats, scopes, global stats, constraints, AVGRATE)  
*area:* leaderboards/tournaments · *musubi:* 🟡 partial (7h/6p/1m over 14 rows)
- **Competitors with it (4):** AccelByte, Hiro/Satori, PlayFab, Steamworks
- **Why it matters:** Progression and matchmaking inputs.
- **Implementation sketch (musubi terms):** Exists: `/v1/server/stats/*` (sum/max/min/last). Gap: global app-wide stats, cycles, constraints (increment-only, max delta), AVGRATE windows.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Global statistics (game-wide accumulated value across all users)* — no game-wide aggregated stat (rg `global_stat` none); only per-player stats + leaderboards
  - 🟡 AccelByte: *Statistic cycles: daily/weekly/monthly/seasonal/annual cycles with reset, per-cycle min/max, cycle-scoped values; incre…* — leaderboard windows/cron resets and quest periods exist, but player stats themselves are not cycle-scoped (no daily/weekly stat values, no per-cycle min/max)
  - 🟡 AccelByte: *Goal requirements: statistic, statistic+cycle, achievement unlocked, login, 3rd-party login, account created (publisher…* — quest objectives driven by stats/events; no built-in account-lifecycle (link/upgrade/login) predicates
  - 🟡 Hiro/Satori: *Public/private player stats* — musubi stats: `POST /v1/server/stats/defs` typed definitions + player stats (match results update); server-key only writes; no public/private split or client-side whitelist semantics (rg "public" stats -> none)
  - 🟡 PlayFab: *Community statistics: title-level aggregation of a source statistic (Sum/Max/Min), one source only, no Last* — musubi: progress graphs/live events via Zanshin; stats are per owner — global aggregate only via storage aggregates or leaderboard sum (no dedicated community stat)
  - 🟡 Steamworks: *Stat types* — player stats defs support folds `sum\|max\|min\|last` (README Player stats); no float moving-average/AVGRATE fold
  - 🟡 Steamworks: *Stat constraints* — stats have scope + visibility + fold; server-only write via `/v1/server/stats`; no "max change per write" or min/max clamps (`rg -i "max_change"` → 0)
- **Already covered rows (7):** AccelByte: Statistic configurations: stat code, default/min/max (0..1.79e308, ne…; PlayFab: Statistics v2 (entity statistics): CreateStatisticDefinition (EntityT…; PlayFab: Statistics metadata per entity statistic (string blob attached to eac…; PlayFab: Transactional writes: UpdateStatistics with TransactionId for idempot…; PlayFab: Legacy player statistics (UpdatePlayerStatistics, GetPlayerStatistics…; PlayFab: Legacy PlayerStatistics: UpdatePlayerStatistics (client allowed via p…; Steamworks: Skill matchmaking via stats

#### P1-16. Game session CRUD (create/describe/search/update/terminate, properties & data, idempotency)  
*area:* matchmaking · *musubi:* 🟡 partial (6h/5p/1m over 13 rows)
- **Competitors with it (4):** AccelByte, Edgegap, GameLift, UGS Multiplayer
- **Why it matters:** Session objects carry game metadata between matchmaking and servers.
- **Implementation sketch (musubi terms):** Add `sessions` with `properties`/`data`, idempotency key, search; link to allocations (musubi-fleet).
- **What the reports found weaker/missing (merged rows):**
  - ❌ GameLift: *Idempotent CreateGameSession* — `rg idempot crates/musubi-server/src/http/fleets.rs` → none (idempotency keys exist on economy routes only)
  - 🟡 Edgegap: *Session lifecycle* — Allocations are recorded on `GameServer.allocation`; no independent session object with lifecycle
  - 🟡 Edgegap: *Session users management* — `POST /v1/gameserver/players` reports players; connect tokens per allocation
  - 🟡 GameLift: *Game properties & data* — allocation `props` JSON passed at allocate; no post-allocate property update API (`rg 'PATCH.*gameserver'` → none)
  - 🟡 GameLift: *Search game sessions* — `GET /v1/server/fleets/{id}/servers`, labels filter; lobby/match listing via `GET /v1/matches`; no expression language
  - 🟡 GameLift: *Update game session* — `POST /v1/gameserver/players` updates player counts; no creation-policy toggle
- **Already covered rows (6):** AccelByte: Persistent sessions: survive DS restarts/empty, member history 24h, D…; AccelByte: Session storage: leader-writable, per-member storage, immutable optio…; AccelByte: Session history & telemetry (sessionhistory service: query past sessi…; GameLift: Create game session; GameLift: Describe game session details; GameLift: Terminate game session

#### P1-17. Pluggable match function (external gRPC/HTTP/WASM MMF, matchmaker override/processor hooks)  
*area:* matchmaking · *musubi:* 🟡 partial (6h/5p/3m over 17 rows)
- **Competitors with it (4):** AccelByte, Agones/OM, Edgegap, Nakama
- **Why it matters:** Studios with bespoke algorithms need full control.
- **Implementation sketch (musubi terms):** Exists: WASM `Scorer` trait. Add per-queue `mmf: {kind: wasm|http|grpc}` that receives the ticket pool and returns proposals (Open Match model) in musubi-matchmaker/mmf.rs.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: ***Match Function as an external service** (gRPC or HTTP, any language): `Run(MatchProfile) → stream Match proposals`* — Matching is in-process Rust: config-driven queue algorithm + optional `Scorer` trait (mm/queue.rs `Scorer`, `with_scorer`) compiled into the binary; WASM modules have no matchmaking hook (`rg -i matchmaker crates/musubi…
  - ❌ Agones/OM: *`InvokeMatchmakingFunctions(MmfRequest{Profile, mmfs[]})` — long-lived bidirectional stream; om-core queries pools and…* — Same gap as OM1 MMF: no external/pluggable match function
  - ❌ Agones/OM: *`open-match-ecosystem` repo with MMF samples and matchmaker examples; Go client modules in `pkg/`* — No sample external matchmakers (not applicable without MMF hook) and no client modules
  - 🟡 Agones/OM: *`extensions map<string, google.protobuf.Any>` — opaque typed payload passed through to MMF and assignment* — Props are JSON scalars; no typed/opaque blob channel documented
  - 🟡 Agones/OM: *Demo matchmaker app (mock frontend, director, MMF, web UI) installable with one command* — README quick start; no bundled demo UI
  - 🟡 Agones/OM: *Tutorials: Matchmaker 101 (frontend/director/MMF scaffolds in Go, build/push images), 102 (role pools), Default Evaluat…* — README/parity docs only; no step-by-step matchmaking tutorials
  - 🟡 Agones/OM: *Horizontal pools of om-core: separate ingestion vs MMF-invocation deployments* — Any node ingests; one leader matches (README:matchmaker) — no dedicated matcher pool scaling
  - 🟡 Nakama: *Matchmaker override function (`RegisterMatchmakerOverride`: reorder/filter candidate matches) and `RegisterMatchmakerPr…* — musubi: named queues with MMR/ping windows widening by wait, teams/roles, ready check, dodge penalty, `simulate`, stats (`/v1/server/matchmaker/queues`, `/stats`) and `hook_matchmaker_matched` to WASM; no hook to replac…
- **Already covered rows (6):** AccelByte: Overridable matchmaking (Extend Override gRPC): GetStatCodes, EnrichT…; Agones/OM: MMF decides to reuse backfill B1 vs create a new one; example `exampl…; Agones/OM: Services `om-frontend`, `om-backend`, `om-query` (3 replicas each, `p…; Agones/OM: Override `ConfigMap om-configmap-override` for all tuning (`api.*`, `…; Agones/OM: Install via YAML (`01-open-match-core.yaml` + override + default eval…; Edgegap: Advanced Matchmaker (OpenMatch on Managed Cluster)

#### P1-18. Server browser (list DS/matches with filters, favourites, history, slots, reputation)  
*area:* matchmaking · *musubi:* 🟡 partial (3h/4p/3m over 10 rows)
- **Competitors with it (4):** Edgegap, Photon, PlayFab, Steamworks
- **Why it matters:** Community servers and custom games.
- **Implementation sketch (musubi terms):** Exists: `GET /v1/matches?label=…`. Add `GET /v1/servers` with tag filters/sort, per-player favourites/history, slots & metadata.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *Unity Server Browser SDK* — No Unity SDK
  - ❌ PlayFab: *Direct peer-to-peer connectivity: PartyDirectPeerConnectivityOptions (SameEntityLoginProvider/AnyPlatformType/AnyEntity…* — WebRTC data channels exist client↔server; no P2P mesh negotiation
  - ❌ Steamworks: *Favorite servers list* — no per-player favourite/history server list (`rg -i favorite` → friends favourite flag only)
  - 🟡 Edgegap: *Community servers (player-provisioned persistent servers)* — Lobbies with password/private code and `start → fleet allocate` (README lobbies row); no owner-bound long-lived server policy
  - 🟡 Edgegap: *Managed Server Browser service* — `GET /v1/matches?label&min_size&max_size&open_slots&region` lists *cluster-hosted* matches and lobbies `/v1/lobbies` list; dedicated-server registry (`/v1/server/fleets/{id}/servers`) is server-key only — no client-faci…
  - 🟡 Photon: *Lobby types* — one lobby model with metadata filters; no async lobby
  - 🟡 Steamworks: *Clan-associated servers* — groups exist, and matchmaker avoid lists exist; no clan-to-server association or compatibility score
- **Already covered rows (3):** Photon: Default lobby room list; Photon: Room list ordering & limits; Photon: Custom room properties + lobby-exposed subset

#### P1-19. Quests / missions / challenges (daily/weekly, rerolls, rotations, randomized goal pools)  
*area:* progression/LiveOps · *musubi:* 🟡 partial (3h/3p/0m over 6 rows)
- **Competitors with it (4):** AccelByte, brainCloud, Metaplay, Hiro/Satori
- **Why it matters:** Baseline retention loop.
- **Implementation sketch (musubi terms):** Exists: quests with rerolls, group challenges. Gap: challenge rotations with randomized goal pools and schedule objects.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *Challenge service (on request): challenge code, time-bound or timeless period, rotation (daily/weekly/monthly/custom/no…* — quests with periods + rerolls (README "Quests"); no randomized-pool goal assignment, no accelerate/retire controls
  - 🟡 Hiro/Satori: *Player-created challenges from templates* — musubi private tournaments with join_code + prizes (0054/0074) are operator-created; players cannot instantiate a tournament from a template or invite friends (rg "challenge" -> group challenges only, server key)
  - 🟡 Hiro/Satori: *Hiro integration: Hiro system definitions (economy, achievements, event leaderboards, teams, incentives, challenges, ma…* — musubi content bundles cover export/import/diff/rollback of definition tables via API; no UI
- **Already covered rows (3):** AccelByte: Extend Override apps: gRPC overrides for matchmaking, loot box roll,…; brainCloud: Milestones & Quests (compound milestones), categories, reset; Metaplay: Quests/achievements/battle pass/seasons/energy/streaks as built-ins

#### P1-20. Streaks / daily login rewards / calendars  
*area:* progression/LiveOps · *musubi:* 🟡 partial (2h/3p/0m over 5 rows)
- **Competitors with it (4):** Beamable, LootLocker, Hiro/Satori, Nakama
- **Why it matters:** Retention.
- **Implementation sketch (musubi terms):** Exists: `/v1/streaks*`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 LootLocker: *Magic Triggers (Oct 2026): automatic rewards on Connects a platform account (specific/any), Daily login, First-time pla…* — Streaks/daily login rewards and `account.linked`/`account.created` events exist (streaks, event bus → modules); no console-configured no-code event→reward rules with delivery-issue log
  - 🟡 Hiro/Satori: *Streak config* — musubi streaks (0017/progression.rs: daily streak with `grace` days, reward per step, reset); missing cron/local-time windows, idle decay amounts, max_count_current_reset, max_count_reached, programmatic reset RPC for s…
  - 🟡 Hiro/Satori: *Streak state fields returned* — musubi `GET /v1/streaks` returns count/last/next; fewer derived flags
- **Already covered rows (2):** Beamable: Calendars (claim daily rewards); Nakama: Economy guide (IAP → premium currency via wallet + storage inventory,…

#### P1-21. Match RPC / messaging (targeted, broadcast, reliable/unreliable, priorities, interest groups)  
*area:* realtime/matches · *musubi:* 🟡 partial (6h/4p/2m over 12 rows)
- **Competitors with it (4):** Colyseus, Meshing vendors, Photon, Rivet
- **Why it matters:** Baseline.
- **Implementation sketch (musubi terms):** Exists: `match_data_send` targeted/broadcast with channels. Gap: per-entity-per-subscriber send priority / distance tiers.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Photon: *Send priority (2.1)* — no per-entity send-rate priority (rg send_rate=0, priority only in queues)
  - ❌ Photon: *PUN 2 (LTS)* — no Unity SDK
  - 🟡 Photon: *TickAligned RPC* — match_input carries tick; generic RPC not tick-aligned
  - 🟡 Photon: *Synchronization & state (PUN)* — typed IDL state; engine binding missing
  - 🟡 Photon: *RPC & RaiseEvent (PUN)* — server RPC exists; "buffered" (cached) RPC absent
  - 🟡 Photon: *RoomOptions* — lobbies: public/private, max players, password, metadata, ready (README lobbies); no PlayerTTL/EmptyRoomTTL knobs, no suppress-events flags
- **Already covered rows (6):** Colyseus: `broadcast(type, msg, {except, afterNextPatch})`, `broadcastBytes`, `…; Meshing vendors: Flood control; Meshing vendors: RPC; Meshing vendors: Cross-server RPCs; Photon: Event forwarding (PathEvent) / WebRPC; Rivet: Typed events: `c.broadcast(name,…)`, `conn.send(name,…)`, client `.co…

#### P1-22. Player search / lookup by id, username, external platform id  
*area:* accounts · *musubi:* 🟡 partial (1h/2p/0m over 3 rows)
- **Competitors with it (3):** AccelByte, EOS, GameLift
- **Why it matters:** Support and social features.
- **Implementation sketch (musubi terms):** Exists: `GET /v1/users/search`, `GET /v1/admin/accounts?identity=`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 EOS: *Accounts dashboard in Developer Portal: search players by external account id or PUID, view keychain of linked accounts…* — admin API /v1/admin/accounts search + identity lookup; no console UI
  - 🟡 GameLift: *Console player session lookup* — admin API only; README states admin console UI is not written
- **Already covered rows (1):** AccelByte: Admin user search by email/display name/username/3rd-party platform i…

#### P1-23. OAuth2 clients / app registrations (client ids, secrets, redirect URIs)  
*area:* auth · *musubi:* 🟡 partial (0h/2p/1m over 3 rows)
- **Competitors with it (3):** AccelByte, EOS, LootLocker
- **Why it matters:** Multiple game clients/tools need distinct credentials and audit.
- **Implementation sketch (musubi terms):** Add `oauth_clients` table + client-credentials grant in musubi-auth; today only `server_keys`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *CRM Platforms (Jun 2026): Brevo (API key) and CleverReach (OAuth client + group, double opt-in key + activation form) —…* — rg -i 'brevo\|cleverreach\|crm' = 0; webhooks on `account.created` could feed a CRM but no built-in connector
  - 🟡 AccelByte: *Security/compliance: ISO 27001 controls stated; HTTPS everywhere, WSS Lobby; GDPR tooling (Private Cloud + APIs in Publ…* — TLS via ingress, GDPR export/delete, audit; no third-party certification (self-hosted)
  - 🟡 EOS: *Developer Portal: org/product/sandbox/deployment hierarchy, roles (Owner/Admin/...), agreements, client credentials, do…* — musubi: multi-project/env config + operator roles via API; no portal UI

#### P1-24. Platform ownership / entitlement check at login (Steam app ownership, Family Sharing)  
*area:* auth · *musubi:* ❌ missing (1h/1p/1m over 3 rows)
- **Competitors with it (3):** EOS, Pragma, Steamworks
- **Why it matters:** Blocks pirated/shared copies and propagates bans to the owner.
- **Implementation sketch (musubi terms):** Add Steam `CheckAppOwnership` call in musubi-auth/steam.rs with family-sharing owner id stored on identity; ban propagation in moderation.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Pragma: *Bans: Steam Family Sharing enforcement* — no Family Sharing / entitlement-owner concept (`rg -i 'family'` → 0).
  - 🟡 EOS: *Client-policy action granularity: Achievements (find/read defs, find/unlock ForLocalUser/ForAnyUser), Anti-Cheat (authe…* — musubi: coarse roles (player JWT vs server key vs admin roles support/liveops/ops) + per-key allowed_paths? (rg "allowed" → scoped keys have scopes list) — not per-action matrix with local/any distinction
- **Already covered rows (1):** Steamworks: Application ownership check

#### P1-25. Login allow-list / limited access / region-locked playtests (compliance side)  
*area:* compliance · *musubi:* 🟡 partial (0h/2p/1m over 3 rows)
- **Competitors with it (3):** Hathora, Photon, Pragma
- **Why it matters:** Controlled launches.
- **Implementation sketch (musubi terms):** See LiveOps "Maintenance / limited access".
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hathora: *Advanced DDoS (Enterprise)* — see Rooms/clientIPs
  - 🟡 Photon: *Region allowlist per AppId* — fleet regions are config-driven; no per-app allowlist concept
  - 🟡 Pragma: *Limited Access Mode (scheduled playtests)* — musubi has bans and a login queue but no shard allow-list/playtest mode (`rg -i 'allow.?list\|whitelist'` not present for logins). Could be emulated with a login hook module.

#### P1-26. Admin API (REST surface for everything the console does)  
*area:* console/admin · *musubi:* 🟡 partial (5h/5p/0m over 12 rows)
- **Competitors with it (3):** LootLocker, Metaplay, UGS LiveOps
- **Why it matters:** Automation.
- **Implementation sketch (musubi terms):** Exists: 93+ `/v1/admin/*` routes.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 LootLocker: *Localization: create/delete localizations, update localization values (per-language strings for broadcasts etc.)* — Audience `lang` targeting + chat translation hooks; no managed localization tables
  - 🟡 LootLocker: *Game Merging: diff two games/progressions, Get Merge Differences, Merge (publisher-level content merge between titles;…* — Content bundle export/import + versions (`/v1/admin/content/*`, `musubi export/import`) move definitions between environments; no interactive diff/merge UI
  - 🟡 LootLocker: *Content taxonomy helpers: Rarities CRUD, Asset Link Types / Asset Links, Maps, Bones (rig bones for cosmetics), Genres,…* — Item tags/kinds/attributes are free-form; no first-class rarity/link/map/bone objects
  - 🟡 LootLocker: *Leaderboard admin: Schedules CRUD, Archives (list/download reset archives), Rewards CRUD, Manual Resets (request/list/g…* — Server routes create/delete boards, authoritative writes, record delete, cron windows, manual `POST /v1/server/leaderboards/{id}/rollover`, windows history; no CSV archive download, no batch member ranks route
  - 🟡 UGS LiveOps: *Scheduler Admin API* — Timers API (create/list/delete/sweep) covers one-shot; recurring needs module redeploy
- **Already covered rows (5):** LootLocker: Data Migration suite: Players, Player Status, Currency, Validate, Met…; LootLocker: Player admin: Search Players, Count Players For Game, Player Platform…; LootLocker: Platforms config API (dated versions, platform settings), Title Confi…; Metaplay: Admin API auth: dashboard SSO (Google/GitHub/Atlassian via Portal), J…; UGS LiveOps: Moderation admin API (incidents, chat logs, player actions, action ty…

#### P1-27. Console access: roles/ACL templates, MFA, SSO, audit trail, team invites  
*area:* console/admin · *musubi:* 🟡 partial (1h/2p/0m over 3 rows)
- **Competitors with it (3):** LootLocker, Nakama, Photon
- **Why it matters:** Secure shared access.
- **Implementation sketch (musubi terms):** Exists: operator SSO + audit log + fixed roles. Add role editor + MFA requirement in console.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 LootLocker: *Rotate Game Key / Rotate Server Key; Get API Keys; Delete API Key; 2FA endpoints; Login/Logout/Signup/Verify Email/Rese…* — Admin/server keys create/revoke (`/v1/admin/keys`), JWT key rotation (`/v1/admin/jwt/rotate`); operator SSO; no explicit rotate-in-place
  - 🟡 Photon: *Dashboard team sharing* — operator SSO + roles via server keys; no UI
- **Already covered rows (1):** Nakama: Console users with roles Administrator / Developer / Maintainer / Vie…

#### P1-28. Health policy (initial delay, period, failure threshold, disabled) and disruption/eviction semantics  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (1h/3p/0m over 4 rows)
- **Competitors with it (3):** Agones/OM, Edgegap, Rivet
- **Why it matters:** Avoid killing live matches during maintenance.
- **Implementation sketch (musubi terms):** Add `health: {initial_delay, period, failures}` + `eviction: never|on_upgrade|always` in FleetSpec mapped to k8s/agones providers.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Agones/OM: *Health check spec `health.disabled`, `initialDelaySeconds` (5), `periodSeconds` (5), `failureThreshold` (3)* — `POST /v1/gameserver/health` updates `last_health`; staleness threshold is hard-coded in the reconciler, not per fleet (`rg -i 'health_period\|failure_threshold'` → 0) and cannot be disabled
  - 🟡 Edgegap: *Active caching (image pre-pulled worldwide)* — Firecracker provider warm snapshots/`warm_pool` restore in tens of ms on *your* hosts (README, `firecracker.rs`); docker/k8s rely on node image cache; no cross-region image pre-distribution
  - 🟡 Rivet: *Pool eviction smoothing: `actorEvictionDelay/Period/Rate`, `drainOnVersionUpgrade` (default true)* — fleet `drain` + `max_concurrent_drains` (fleets.rs); module canary/promote/rollback (README L33) but no eviction rate controls for live matches
- **Already covered rows (1):** Agones/OM: Controller liveness probe settings (`healthCheck.initialDelaySeconds/…

#### P1-29. Rolling updates / build canary / promote with maxSurge/maxUnavailable, rotate Ready servers  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (2h/3p/0m over 7 rows)
- **Competitors with it (3):** Agones/OM, Edgegap, GameLift
- **Why it matters:** Zero-downtime patches.
- **Implementation sketch (musubi terms):** Exists: canary percent/promote. Add rolling strategy that replaces Ready servers under maxSurge/maxUnavailable and auto-rollback on health failures.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Agones/OM: *`strategy.type: RollingUpdate` with `maxSurge` (25 %) / `maxUnavailable` (25 %), never touches `Allocated` servers; `Ro…* — Build promotion (`POST /v1/server/fleets/{id}/builds/{v}/promote`) changes what **new** servers run; running Ready servers are not rotated, no surge/unavailable knobs, no Recreate
  - 🟡 Agones/OM: *Upgrade guides: multi-cluster blue/green (recommended), single-cluster fleet drain; feature-gate changes documented per…* — Rolling node restarts + `musubi migrate` (README:operating); no written upgrade runbook for fleets during a control-plane upgrade
  - 🟡 GameLift: *Container fleet deployments* — builds with `canary_percent`/`canary_regions` + `promote`; no min-healthy % or impairment auto-rollback
- **Already covered rows (2):** Edgegap: App Version = immutable named build config; Edgegap: Blue/green & canary

#### P1-30. Secrets management (encrypted at rest, secret manager hierarchy, roles, SCIM)  
*area:* deployment/ops · *musubi:* 🟡 partial (0h/3p/0m over 3 rows)
- **Competitors with it (3):** Hiro/Satori, Nakama, UGS LiveOps
- **Why it matters:** Compliance for credentials.
- **Implementation sketch (musubi terms):** Add `MUSUBI__*__secret_ref` indirection (env/file/Vault/KMS) in musubi-core config; encrypt stored provider credentials.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Hiro/Satori: *Enterprise SSO (SAML) + SCIM directory sync groups → teams* — musubi operator SSO/OIDC (parity.md) without SCIM
  - 🟡 Nakama: *Enterprise SSO (SAML via WorkOS, domain-based routing, multiple domains) + SCIM directory sync mapping IdP groups → tea…* — operator SSO via OIDC exists; no SAML (parity.md "SAML missing"), no SCIM (rg -i scim → none)
  - 🟡 UGS LiveOps: *Secret Manager (org/project/env hierarchy, inheritance, override; 50 secrets per level; value ≤50,000 chars; roles)* — `[runtime.secrets]` from env/files plus typed per-module env/secrets declared in the IDL (`[env."module/KEY"]`, required/default/secret, sealed at rest, redacted in the API, publish gate on unbound required keys — P1-63); no org/project hierarchy

#### P1-31. Reward tables, drop tables, loot rolls, reward modifiers, grant limits  
*area:* economy/IAP · *musubi:* 🟡 partial (2h/3p/0m over 5 rows)
- **Competitors with it (3):** AccelByte, Hiro/Satori, PlayFab
- **Why it matters:** Core progression plumbing.
- **Implementation sketch (musubi terms):** Exists: reward tables, rolls, modifiers, grant limits. Gap: item-set (tag) rolls, weighted instance-property rolls, modifiers grantable as rewards.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *Reward (12): reward configs by event (statistic/achievement/user account), conditions, export/import; ServicePluginConf…* — reward tables + hooks; no event-rule reward configs
  - 🟡 Hiro/Satori: *Reward modifiers (temporary multiplier/addition on future rolls)* — README `PUT /v1/server/rewards/modifiers {id, scope, factor, starts_at, ends_at, account_id?}` + `GET /v1/rewards/modifiers`: operator-set modifiers but only multiplicative factor, not grantable as reward contents (play…
  - 🟡 Hiro/Satori: *Energy / energy modifiers as reward contents* — musubi energy grant via `POST /v1/server/energy/{id}/grant`; rg "energy" in rewards.rs -> not a reward entry kind
- **Already covered rows (2):** AccelByte: Monetization customization: per-feature Extend app or own gRPC server…; PlayFab: Legacy drop tables: weighted random result tables, nested tables, con…

#### P1-32. User-generated content (upload, moderation queue, ratings, subscriptions, share codes, dependencies, versioning)  
*area:* economy/IAP · *musubi:* 🟡 partial (4h/3p/1m over 9 rows)
- **Competitors with it (3):** AccelByte, LootLocker, Steamworks
- **Why it matters:** Community content extends lifetime.
- **Implementation sketch (musubi terms):** Exists: `/v1/ugc*` (blobs ≤16 MiB, submit/approve, rate, subscribe). Gap: share codes, dependencies/collections, versioning vs builds, >16 MiB resumable uploads, creator revenue, type taxonomy.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Steamworks: *Paid Workshop items & creator revenue* — no revenue share accounting for creators
  - 🟡 AccelByte: *UGC API (160): admin channels/content v1+v2/groups (content groups)/tags/types/config/staging (approval queue approve-r…* — UGC core have; content groups, staging approval, share codes, follow, download counts missing
  - 🟡 LootLocker: *Player / UGC / Game feedback reports with console-managed categories per type, player-supplied description, console vie…* — `POST /v1/reports` (player reports, categories, moderation queue `/v1/admin/reports`), chat/voice reports, UGC reports; no "game feedback/bug report" category type and no console UI
  - 🟡 Steamworks: *Workshop models* — UGC registry with moderation workflow (draft → review → public), ratings, subscriptions (README UGC); no CDN distribution to game clients and no creator payouts
- **Already covered rows (4):** AccelByte: UGC v2: channels (per creator; group by region/build/language), conte…; Steamworks: Tags administration; Steamworks: Moderation Web API; Steamworks: Remote Storage published-file Web API (legacy)

#### P1-33. Session objects (game session lifecycle, player sessions, reservations, seats, TTL)  
*area:* matchmaking · *musubi:* 🟡 partial (4h/4p/0m over 9 rows)
- **Competitors with it (3):** Colyseus, Edgegap, GameLift
- **Why it matters:** GameLift/Edgegap-style session accounting for DS flows.
- **Implementation sketch (musubi terms):** Add `sessions` table linking allocations ↔ players with reserve/confirm/expiry (30 s) in musubi-fleet; expose `/v1/sessions*`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Edgegap: *Seat reservation* — Lobby join + connect tokens; no seat-count reservation with expiry against dedicated servers
  - 🟡 GameLift: *Game session placement via queue* — `POST /v1/server/allocate{pool,region}` is synchronous (waits for cold start); no async placement object with id/status/stop
  - 🟡 GameLift: *Game session object* — `GameServer` + `Allocation`; no status reason enum, no creator id
  - 🟡 GameLift: *Player session object* — connect token + `players` report; no persisted player-session entity with timestamps
- **Already covered rows (4):** Colyseus: `publicAddress` per process (seat reservation carries it so SDK conne…; Colyseus: `devMode`: restore rooms + state + seat reservations after code-chang…; Colyseus: Join paths: `joinOrCreate`, `join`, `joinById`, `create`, `reconnect`…; GameLift: Matched player session object

#### P1-34. Interest management (radius, multi-query, tag/global queries, per-query frequency, byte budgets)  
*area:* meshing/world · *musubi:* 🟡 partial (3h/3p/1m over 7 rows)
- **Competitors with it (3):** Meshing vendors, Photon, UGS Multiplayer
- **Why it matters:** Bandwidth under control.
- **Implementation sketch (musubi terms):** Exists: single radius + filter + `budget_bytes`. Add composable queries (and/or/not, tag, global) with per-query frequency.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Meshing vendors: *Streaming queries (deprecated)* — no distant/global interest
  - 🟡 Meshing vendors: *Tag Query* — component-value `filter` on subscribe approximates tags but still radius-bound
  - 🟡 Meshing vendors: *Query-based interest* — radius + component-value filter + max_hz per client; no composable constraints, no per-query frequency, no server-worker interest config beyond ghost margin
  - 🟡 Photon: *Interest Management addon* — cell/AOI radius based; no shape library
- **Already covered rows (3):** Meshing vendors: Edge networking nodes; Meshing vendors: Interest management; UGS Multiplayer: Object visibility / interest management

#### P1-35. Simulated player load tests / deployment load testing  
*area:* meshing/world · *musubi:* 🟡 partial (2h/2p/0m over 4 rows)
- **Competitors with it (3):** Hathora, Meshing vendors, UGS Multiplayer
- **Why it matters:** Capacity validation.
- **Implementation sketch (musubi terms):** Exists: `musubi bench --scenario world`, `musubi soak`. Gap: engine-driven simulated players.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Meshing vendors: *Simulated players* — `musubi bench`/`soak` synthetic load, not engine-driven bot clients
  - 🟡 Meshing vendors: *Simulated players* — `bench`/`soak` synthetic load
- **Already covered rows (2):** Hathora: Load/soak tooling; UGS Multiplayer: Thin clients for soak/stress

#### P1-36. Custom HTTP routes / handlers from module code (+ middleware, CORS, basic auth)  
*area:* runtime/extensibility · *musubi:* 🟡 partial (0h/3p/0m over 3 rows)
- **Competitors with it (3):** Nakama, Rivet, UGS LiveOps
- **Why it matters:** Backends need bespoke endpoints (webhooks, launchers).
- **Implementation sketch (musubi terms):** Add `http_<name>` module exports mounted under `/v1/x/{module}/{name}` with method/auth metadata in the IDL.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Nakama: *Scheduler callbacks: `RegisterLeaderboardReset`, `RegisterTournamentEnd`, `RegisterTournamentReset`, `RegisterShutdown`…* — leaderboard/tournament hooks + timers/workflows (`/v1/workflows`, `0098_workflows`, `0101_timers`) exist; no shutdown hook, no raw HTTP handler registration from modules (rg -i register_http → none)
  - 🟡 Rivet: *Cron per actor: `c.cron.set({name, expression, action, args, timezone, maxHistory})`, list/delete/history* — cron exists for leaderboard resets, tournaments, live events, retention (`schedules.rs`, README L222) but not as a user-programmable per-actor cron with history
  - 🟡 UGS LiveOps: *Filters (CEL expressions; dashboard offers ≤10 simple filters with AND/OR)* — Webhook event-type filters; module hooks filter in code; no CEL engine on the bus (`cel` hits are unrelated substrings)

#### P1-37. MCP server / AI agent skills / LLM-friendly docs  
*area:* runtime/extensibility · *musubi:* 🟡 partial (0h/1p/0m over 3 rows)
- **Competitors with it (3):** AccelByte, brainCloud, Rivet
- **Why it matters:** 2026 developer workflow: agents configure backends.
- **Implementation sketch (musubi terms):** Add `musubi mcp` (stdio server over the admin API + docs) and `llms.txt` on the docs site.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *Extend ops: 1–60 replicas, CPU/memory per app, autoscale on CPU target 30–90% (default 50%), per-namespace or all-names…* — modules run in-process (fuel/memory limits, hot reload, canary) — no per-module replica autoscaling, no vuln scanning

#### P1-38. Query / index / search over storage (filters, sort, aggregate, full-text, random sample)  
*area:* storage · *musubi:* 🟡 partial (2h/2p/0m over 4 rows)
- **Competitors with it (3):** Hiro/Satori, Nakama, UGS LiveOps
- **Why it matters:** Leaderboard-like and marketplace queries over save data.
- **Implementation sketch (musubi terms):** Exists: `where`/`sort`/`aggregate`/`$q` in musubi-storage. Gap: random sampleSize, fuzzy/regex/boosting, per-index filter callback.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Nakama: *Storage Search indices: runtime-registered per collection (+optional key), top-level `fields`, `sortableFields`, `maxEn…* — musubi: Postgres-backed `where` filters + GIN indexes, full-text search (parity.md "indexes/full-text"), `aggregate`; no in-memory Bleve-style index with boosting/fuzzy query syntax (rg -i bleve → none)
  - 🟡 UGS LiveOps: *Random sampling in queries* — `rg -i sample` shows seeded RNG and bench sampling, no `sample` query option on storage list — sketch: `?sample=N` using `TABLESAMPLE`/`ORDER BY random()` with limit
- **Already covered rows (2):** Hiro/Satori: Observability: graphs (LB status codes, CPU/mem per node, DB CPU/quer…; UGS LiveOps: Full-text / aggregate queries

#### P1-39. Client incident / crash reports / SDK error reports  
*area:* analytics/observability · *musubi:* ❌ missing (1h/0p/1m over 2 rows)
- **Competitors with it (2):** Rivet, Steamworks
- **Why it matters:** Find client-side failures fast.
- **Implementation sketch (musubi terms):** Add `POST /v1/telemetry/crash` + `POST /v1/telemetry/errors` (dedupe by fingerprint, retention) in a new telemetry module.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Steamworks: *Crash / minidump collection* — no crash dump ingestion endpoint
- **Already covered rows (1):** Rivet: Crash reporting/telemetry opt-out `RIVET__TELEMETRY__ENABLED=false`

#### P1-40. Per-player event history / CS timeline  
*area:* analytics/observability · *musubi:* ❌ missing (0h/1p/2m over 3 rows)
- **Competitors with it (2):** Metaplay, PlayFab
- **Why it matters:** Support investigations.
- **Implementation sketch (musubi terms):** Add `GET /v1/admin/accounts/{id}/events` over the events table with filters and retention.
- **What the reports found weaker/missing (merged rows):**
  - ❌ PlayFab: *PlayStream Monitor / Event History / PlayStream Debugger: live firehose view and per-player event history in Game Manag…* — No stored event history or live event viewer; events only forwarded to webhooks/Zanshin
  - ❌ PlayFab: *Event History (per player Event History tab; ElasticSearch 30-day TTL) and GetPlayerEventHistory-style views, PlayStrea…* — None (audit log covers admin actions only)
  - 🟡 Metaplay: *Social login conflict resolution event in player event log* — account merge exists; no per-player event log of conflict resolutions

#### P1-41. Profile field edit restrictions and input validation rules (username regex, cadence)  
*area:* auth · *musubi:* ❌ missing (1h/1p/1m over 3 rows)
- **Competitors with it (2):** AccelByte, Pragma
- **Why it matters:** Prevents impersonation churn; trust & safety ask.
- **Implementation sketch (musubi terms):** Add per-field cadence + regex policy in `[account.validation]` (musubi-account).
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Profile edit restrictions per field: unlimited / interval (e.g. every 7/180 days) / creation-only* — no per-field edit cadence policy
  - 🟡 AccelByte: *IAM input validation: username/display name/password/email/avatar rules (min/max length, regex, special chars, unicode,…* — password policy + display-name word filter exist (musubi-auth config, chat word filters reused for names: rg `display_name` filter); no admin-configurable regex/special-char/unicode rule set
- **Already covered rows (1):** Pragma: Display name rules & update

#### P1-42. API explorer / request console / data reset tools  
*area:* console/admin · *musubi:* 🟡 partial (1h/2p/0m over 3 rows)
- **Competitors with it (2):** brainCloud, Nakama
- **Why it matters:** Developer convenience.
- **Implementation sketch (musubi terms):** Swagger UI over `/openapi.json` inside the console + `musubi reset --env`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *API Explorer / S2S Explorer* — `GET /openapi.json` → Swagger/any client; no hosted explorer
  - 🟡 Nakama: *API Explorer (run any endpoint/RPC as a chosen user id with session vars, persistent tabs); Runtime Modules view (loade…* — `GET /openapi.json` + `musubi export/import`, module list routes (`/v1/admin/modules`); no explorer/reset UI
- **Already covered rows (1):** Nakama: Console API (gRPC :7348 / HTTP `/v2/console/*`, 94 RPCs: authenticate…

#### P1-43. Counters / Lists / per-server metadata (capacities, allocation filters, autoscaler targets)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (2):** Agones/OM, Steamworks
- **Why it matters:** Generic occupancy tracking beyond players.
- **Implementation sketch (musubi terms):** Add `counters{}`/`lists{}` on gameservers with capacities; usable as allocation filters and autoscaler targets (musubi-fleet).
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Agones/OM: *Policy `Counter` / `List`: keep `bufferSize` of aggregate counter/list capacity free across the fleet, `minCapacity`/`m…* — Demand EWMA uses `players`/`capacity` only (mig 0050 `fleet_demand`); no capacity-based target expressed in slots
- **Already covered rows (1):** Steamworks: Server metadata

#### P1-44. Deployment / gameserver lifecycle webhooks and events (ready/error/terminated)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (1h/2p/0m over 3 rows)
- **Competitors with it (2):** Agones/OM, GameLift
- **Why it matters:** Integrations and ops alerts.
- **Implementation sketch (musubi terms):** Emit `gameserver.ready/allocated/unhealthy/terminated` through the existing webhook dispatcher.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 GameLift: *Fleet events* — event bus + webhooks emit `fleet.build_added`, `fleet.build_promoted`, `match.*`; no per-fleet event log API with codes
  - 🟡 GameLift: *Fleet event object* — audit log + webhooks; no fleet event stream with log URLs
- **Already covered rows (1):** Agones/OM: Octops Event Broadcaster / Broadcaster HTTP / Relay HTTP — push GameS…

#### P1-45. Infrastructure-as-code / declarative resources (CRDs, Terraform provider, kubectl-apply model)  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (1h/2p/2m over 6 rows)
- **Competitors with it (2):** Agones/OM, Colyseus
- **Why it matters:** Reproducible environments.
- **Implementation sketch (musubi terms):** Exists: `musubi deploy` manifest (modules+fleets). Add Terraform provider + idempotent apply for queues/worlds/configs.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *`@colyseus/yjs` (0.18): Yjs CRDT connection provider — room plugin + ready-made room + client provider mirroring `y-web…* — `rg -i 'crdt\|yjs'` → nothing; storage has 3-way merge but no CRDT doc sync
  - 🟡 Agones/OM: *Fleet status: `replicas`, `readyReplicas`, `reservedReplicas`, `allocatedReplicas`, aggregated `counters`/`lists`* — `GET /v1/server/fleets/{id}` + `musubi_fleet_servers{fleet,state}` gauge; no reserved count, no aggregated counters
  - 🟡 Agones/OM: *System diagram documentation of all deployments, CRDs and data flow* — README architecture prose; no diagram
- **Already covered rows (1):** Agones/OM: CRD API reference (generated HTML for every field of every CRD)

#### P1-46. Rooms-per-process packing / multi-match per server  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (0h/2p/0m over 2 rows)
- **Competitors with it (2):** GameLift, Hathora
- **Why it matters:** Cuts DS cost by 2–10×.
- **Implementation sketch (musubi terms):** Add `slots_per_server` in FleetSpec; allocation packs N matches onto one server before cold-starting another.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 GameLift: *Container capacity packing* — k8s scheduler packs pods; musubi only sets `vcpu` for accounting
  - 🟡 Hathora: *Multiple rooms per process (roomsPerProcess)* — fleet allocation is one allocation per game server (`/v1/gameserver/allocate`, `allocate_routed`); in-process authoritative matches (`musubi-actor`) pack unlimited matches per node — but no per-server slot count for ded…

#### P1-47. Server inventory / search API (filter by region/status/age, counts, exit reason)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (2):** Edgegap, UGS Multiplayer
- **Why it matters:** Operators need to find servers.
- **Implementation sketch (musubi terms):** Exists: `GET /v1/server/fleets/{id}/servers`. Add cross-fleet `GET /v1/server/gameservers?region&status&created_before` + exit reasons.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 UGS Multiplayer: *Server query protocols SQP / A2S* — `POST /v1/gameserver/players {players, capacity}` occupancy push; no SQP/A2S listener.
- **Already covered rows (1):** Edgegap: List deployments with pagination

#### P1-48. Economy configuration workflow (draft/publish, catalog versions, per-locale strings, images, reviews)  
*area:* economy/IAP · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (2):** PlayFab, UGS LiveOps
- **Why it matters:** Safe catalog changes.
- **Implementation sketch (musubi terms):** Exists: item versions/rollback. Add draft→publish stage on store/items + per-locale strings + images.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 UGS LiveOps: *Draft vs Published configuration, publish step* — Content bundles with versions/diff/rollback (`/v1/admin/content/*`) cover catalogue data; item/store definitions are live-edited via `/v1/server/items / stores` without a draft stage
- **Already covered rows (1):** PlayFab: UGC (Economy v2 user-generated content): players CreateDraftItem/Publ…

#### P1-49. Energy / stamina / time-gated resources  
*area:* economy/IAP · *musubi:* ❌ missing (1h/1p/2m over 4 rows)
- **Competitors with it (2):** Hiro/Satori, SpacetimeDB
- **Why it matters:** F2P pacing.
- **Implementation sketch (musubi terms):** Exists: `/v1/energy*`. Gap: `max_overfill`, reward-on-spend.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hiro/Satori: *Energy modifiers (infinite, set_refill_count, set_refill_sec, set_max_count) with duration* — rg -i "infinite\|modifier" in energy code -> none
  - ❌ Hiro/Satori: *Spend with refill start time (offline)* — no offline reconciliation in musubi
  - 🟡 Hiro/Satori: *Energies: start/max/max_overfill/refill_count/refill_time_sec/implicit/reward on spend* — README energy: `GET /v1/energy`, `POST /v1/energy/{id}/spend`, regen per interval, cap; no overfill cap distinct from max, no reward-on-spend (rg "overfill\|overflow" -> none)
- **Already covered rows (1):** SpacetimeDB: Reducer result callbacks

#### P1-50. Item lifecycle & effects (publish/retire/revoke states, cooldowns, timed status effects, durability)  
*area:* economy/IAP · *musubi:* 🟡 partial (0h/1p/0m over 2 rows)
- **Competitors with it (2):** Metaplay, SpacetimeDB
- **Why it matters:** Deeper item systems without custom code.
- **Implementation sketch (musubi terms):** Add `item_defs.status` (draft/published/retired), `use_cooldown_secs`, `effects[]` with expiry in musubi-economy/items.rs.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Metaplay: *Offer group fields: `Enabled`, `Placement` (one active group per placement), `Priority` (lowest wins), `Lifetime`, `Coo…* — store windows have cron/tz scheduling and rotation; no player-local schedule, cooldown, priority per placement

#### P1-51. Marketplace / auction house / order book  
*area:* economy/IAP · *musubi:* 🟡 partial (1h/2p/1m over 4 rows)
- **Competitors with it (2):** Hiro/Satori, PlayFab
- **Why it matters:** Player-driven economies.
- **Implementation sketch (musubi terms):** Exists: `/v1/market*` (listings, bids, order book).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hiro/Satori: *Anti-snipe extension: extension_threshold_sec / extension_sec / extension_max_sec* — rg -i "extension\|anti.?snipe" market.rs -> none
  - 🟡 Hiro/Satori: *Restricted listings (allowed player ids) for direct trades; bid with items* — musubi P2P trades (`/v1/trades`: offer items+currency to a named player, accept/decline) cover direct trade; market bids are currency-only, no allow-list on listings (rg "allowed" market.rs -> none)
  - 🟡 Hiro/Satori: *Follow an auction (realtime stream of bid updates); list own bids / created; claim bid / claim created; cancel (OnCance…* — musubi `GET /v1/market/listings/mine`, cancel, settle sweep; no per-listing realtime follow stream (storage subscriptions exist generically)
- **Already covered rows (1):** PlayFab: Catalog v2 items: individual versioned items (Draft→Publish, ETags),…

#### P1-52. Redemption codes / key campaigns / platform key inventories  
*area:* economy/IAP · *musubi:* ❌ missing (0h/1p/1m over 2 rows)
- **Competitors with it (2):** AccelByte, brainCloud
- **Why it matters:** Marketing & influencer campaigns.
- **Implementation sketch (musubi terms):** Add `redemption_codes` (single-use generated/shared multi-use, reward payload, redemption history) + `POST /v1/codes/redeem` in musubi-economy; coupons only discount prices today.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Campaign (17 ops) + KeyGroup (8) + Ticket booth (5): code campaigns, redeem, batch key upload (Steam keys etc.) distrib…* — no key-group/key distribution (3rd-party key fulfilment)
  - 🟡 brainCloud: *Redemption codes* — store coupons (`/v1/server/store/coupons`, `POST /v1/store/buy {coupon}`) give discounts only; no code campaigns granting rewards, single-use code generation, or redemption history

#### P1-53. Rewarded video / ad placements with S2S verification  
*area:* economy/IAP · *musubi:* ❌ missing (0h/0p/2m over 2 rows)
- **Competitors with it (2):** Hiro/Satori, PlayFab
- **Why it matters:** Ad-monetized F2P.
- **Implementation sketch (musubi terms):** Add `POST /v1/ads/callback/{provider}` S2S verifiers (ironSource/AppLovin MAX) → reward table grant in musubi-economy.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hiro/Satori: *Rewarded video placements* — rg -i "applovin\|ironsource\|placement\|rewarded" -> none
  - ❌ PlayFab: *Rewarded ads: AdPlacement config, GetAdPlacements, RewardAdActivity, RecordAdActivity, per-player daily reward caps* — No ad placement/reward tracking (grep "ad_placement rewarded": none)

#### P1-54. Group treasury / shared inventory / donations / contribution pools  
*area:* groups · *musubi:* ❌ missing (0h/1p/3m over 4 rows)
- **Competitors with it (2):** Beamable, Hiro/Satori
- **Why it matters:** Guild economies drive retention.
- **Implementation sketch (musubi terms):** Exists: treasury + inventory. Gap: contribution pools/team gifts and team store from treasury (Hiro).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *`GroupDonationsContent` donation rules* — No donation feature
  - ❌ Hiro/Satori: *Team store with reward modifiers (promo events)* — rg "group.*store\|treasury.*buy" -> none; group treasury cannot buy store items
  - ❌ Hiro/Satori: *Team gifts (raid-boss / community drive contribution pools)* — rg -i "gift" -> none; group treasury deposits are not tiered contribution events with member rewards
  - 🟡 Beamable: *Group donation requests* — Group treasury deposit/withdraw exists (`/v1/groups/{id}/treasury`) but no member-to-member donation request flow

#### P1-55. Offline / rating-based async matchmaking (defender selection)  
*area:* matchmaking · *musubi:* 🟡 partial (0h/2p/0m over 2 rows)
- **Competitors with it (2):** brainCloud, Nakama
- **Why it matters:** Asynchronous PvP genre.
- **Implementation sketch (musubi terms):** Add `GET /v1/matchmaker/opponents?queue&mmr` from rating table with MMR bucket cache (musubi-matchmaker/async.rs).
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *Offline matchmaking (rating-based)* — realtime matchmaker with MMR/roles/ping/parties covers rating; no “find opponent candidates for offline attack” API or attack shields (grep `shield` tests only)
  - 🟡 Nakama: *Offline matchmaking pattern via Storage Index + `storageIndexList` with same query syntax (`RegisterStorageIndex`, `Reg…* — musubi `GET /v1/storage/{collection}?where=` filters + full-text over Postgres can select offline opponents, but no per-index filter callback or boosting

#### P1-56. Player-initiated private/custom matches and invitation codes  
*area:* matchmaking · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (2):** Hathora, UGS Multiplayer
- **Why it matters:** Friends play.
- **Implementation sketch (musubi terms):** Exists: lobby join codes, private tournaments. Verify invite-code flow for authoritative matches.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 UGS Multiplayer: *Join codes* — 6-letter lobby codes exist, but tied to lobbies not to a relay allocation.
- **Already covered rows (1):** Hathora: Short codes / join codes

#### P1-57. Spatial partitioning (grid/quadtree/octree, 3-D cells, split/merge by load)  
*area:* meshing/world · *musubi:* ✅ covered (3-D/octree + Morton shipped; 2 rows remain partial)
- **Competitors with it (2):** Meshing vendors, Pragma
- **Why it matters:** Single-shard open worlds.
- **Implementation sketch (musubi terms):** 2-D grid + quadtree with cost-based split/merge, and `three_d: true` worlds: `cell_size` cubes, octree split into 8 / merge of 8, `z` in `Pos`/`CellId` (omitted on the wire when 0), sphere interest, 26 neighbours, vertical ghost/handover borders, `CellId::morton()` Z-order codes.
- **What the reports found weaker/missing (merged rows):**
  - ✅ Meshing vendors: *3-D octree partitioning* — `WorldConfig::three_d` (README "3-D worlds")
  - 🟡 Meshing vendors: *Space→process mapping* — cells (quadtree/octree leaves) leased to simulators; Morton codes exist for ordering/packing but the controller still assigns whole cells, not arbitrary Morton intervals
  - ✅ Meshing vendors: *Morton-region spatial library* — `CellId::morton()` (21 bits per axis, negative coordinates offset) used by the inspector ordering and available to embedders
  - 🟡 Meshing vendors: *Zone System* — attach/detach keeps passengers with transport, but no nested moving coordinate frames
  - 🟡 Pragma: *Multi-database sharding of services* — single Postgres (read replicas via connection string); table partitioning for ledgers/events but no per-service DB routing.
- **Already covered rows (1):** Meshing vendors: Spatial partitioning / load balancing across simulators

#### P1-58. World inspector / spatial debugger / worker load view  
*area:* meshing/world · *musubi:* ❌ missing (0h/0p/3m over 4 rows)
- **Competitors with it (2):** Meshing vendors, Photon
- **Why it matters:** Operating a meshed world blind is impossible.
- **Implementation sketch (musubi terms):** Add admin world view endpoints (exists: `/v1/admin/worlds/*`) + web inspector in the console.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Meshing vendors: *No-code layer / Configure window* — no engine tooling
  - ❌ Meshing vendors: *Spatial Debugger* — `GET /v1/admin/worlds/{w}/entities` + `view/{x}/{y}` data only; no visual tool
  - ❌ Meshing vendors: *Worker Inspector window & Inspector V2* — no inspector UI

#### P1-59. Generic streams primitive for module code (join/leave/send/count on arbitrary streams)  
*area:* realtime/matches · *musubi:* ❌ missing (0h/0p/1m over 2 rows)
- **Competitors with it (2):** GameLift, Nakama
- **Why it matters:** Nakama modules fan out to arbitrary subscriber sets.
- **Implementation sketch (musubi terms):** Expose `stream_*` host ops in musubi-runtime over the realtime subscription registry.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Nakama: *Streams API: 4-part id (mode 0–255, subject, subcontext, label), built-in modes (notifications 0, status 1, chat 2, gro…* — no generic pub/sub stream primitive for module code (rg -i "stream" in musubi-runtime → none)

#### P1-60. Lag compensation / rewind queries (hitbox history)  
*area:* realtime/matches · *musubi:* ❌ missing (1h/0p/1m over 2 rows)
- **Competitors with it (2):** Photon, UGS Multiplayer
- **Why it matters:** Shooter correctness.
- **Implementation sketch (musubi terms):** Exists: world `rewind` (positions). Add hitbox ring buffer + raycast/sphere queries in musubi-sim-sdk.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Photon: *Lag compensation* — server rollback exists but no hitbox history/rewind queries (rg hitbox=0); game code would implement in WASM/game server
- **Already covered rows (1):** UGS Multiplayer: Server-side rewind (lag compensation)

#### P1-61. Match / room lifecycle webhooks & server-to-server callbacks (create/join/leave/close/custom events)  
*area:* realtime/matches · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (2):** Colyseus, Edgegap
- **Why it matters:** Studios mirror room state into their own systems.
- **Implementation sketch (musubi terms):** Exists: webhook system (`/v1/admin/webhooks`). Ensure `match.*`/`lobby.*`/`gameserver.*` events are emitted.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Edgegap: *Lifecycle webhooks* — Outbound webhooks exist for account/match events with HMAC, backoff, DLQ, redelivery (README `/v1/admin/webhooks`), but no `gameserver.ready/terminated` events (`rg on_ready\|on_terminat` → none) — musubi's delivery gua…
- **Already covered rows (1):** Colyseus: Room-type lifecycle events for monitoring: create/dispose/join/leave/…

#### P1-62. Simulated latency / network condition testing  
*area:* realtime/matches · *musubi:* ❌ missing (0h/1p/1m over 2 rows)
- **Competitors with it (2):** Colyseus, Photon
- **Why it matters:** Developers test bad networks.
- **Implementation sketch (musubi terms):** Add `[dev] simulate_latency_ms/jitter/loss` in musubi-edge for dev mode + `musubi bench` flags.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Colyseus: *Browser debug panel (`debug.js`, Shadow DOM): network-condition simulation (RTT + jitter sliders/presets, `__net()`), a…* — no client tooling; server-side `musubi soak` injects packet loss/latency at node level only
  - 🟡 Photon: *Network simulation (editor)* — server/soak-side impairment only

#### P1-63. Module deployment: hot reload, versions, canary, rollback, env vars, local debugger, templates/unit-test scaffolds  
*area:* runtime/extensibility · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (2):** Metaplay, SpacetimeDB
- **Why it matters:** Safe iteration.
- **Implementation sketch (musubi terms):** Exists: publish/canary/promote/rollback, hot reload. Gap: typed per-module env/secrets, local debugger story, `musubi module new --template`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Metaplay: *Runtime options: layered YAML files (`Options.base.yaml`, per-env), env var / CLI overrides, hot reload for marked sect…* — TOML + env overrides, `musubi config` CLI, limited hot reload (`rg -i reload` config); no per-key source view
- **Already covered rows (1):** SpacetimeDB: Hot-swap module publish w/o disconnecting clients

#### P1-64. Game config / remote config / title data (typed, versioned, per-environment)  
*area:* storage · *musubi:* ❌ missing (0h/1p/2m over 3 rows)
- **Competitors with it (2):** Beamable, Metaplay
- **Why it matters:** Live tuning is the heart of LiveOps.
- **Implementation sketch (musubi terms):** Exists: `/v1/configs`, content bundles with versions/rollback. Gap: spreadsheet/CSV ingestion, variants, side-by-side diff, promotion between envs.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Metaplay: *Unity Game Config Build Window; config hot-load into running offline client; references in live models re-pointed* — no engine tooling
  - ❌ Metaplay: *Server-side (remote) config builds from the dashboard; async build with status Success/Failed/Building; name/descriptio…* — no build pipeline
  - 🟡 Beamable: *Content manifest public/private, checksum, pull* — Bundle versions + diff; no public/private manifest split or checksum endpoint

#### P1-65. Title / global files and CDN content delivery  
*area:* storage · *musubi:* ❌ missing (0h/0p/3m over 4 rows)
- **Competitors with it (2):** brainCloud, Metaplay
- **Why it matters:** Ship patches/config/assets without client update.
- **Implementation sketch (musubi terms):** Add title-file tier to `musubi-files` with CDN URLs; content bundles already exist for config.
- **What the reports found weaker/missing (merged rows):**
  - ❌ brainCloud: *Global Files V3 (app assets, folders)* — no asset/file service (content bundles are definition tables, not binaries)
  - ❌ Metaplay: *Client redirect: clients newer than active range are redirected to alternate host/port/CDN (test new version against st…* — no redirect-by-version in login response
  - ❌ Metaplay: *Localization system: translation spreadsheets (`TranslationId` + language columns, `//` comments), `Languages` config l…* — no localization pipeline; only per-message translation in chat (README chat row)

### 3.3 P2 — 46 items

#### P2-1. Managed cloud / hosted offering, pricing, free tier, regions  
*area:* deployment/ops · *musubi:* ❌ missing (3h/4p/5m over 22 rows)
- **Competitors with it (10):** brainCloud, Colyseus, Edgegap, Meshing vendors, Metaplay, Hiro/Satori, Nakama, Rivet, Steamworks, UGS Multiplayer
- **Why it matters:** Many studios do not want to operate anything.
- **Implementation sketch (musubi terms):** Business decision: hosted alpha + region pack (Terraform) + CCU-per-node benchmarks.
- **What the reports found weaker/missing (merged rows):**
  - ❌ brainCloud: *Dormant account billing & auto-delete* — no dormant policy
  - ❌ Edgegap: *MCP server (`mcp.edgegap.dev`, beta)* — `rg -i mcp` → none
  - ❌ Meshing vendors: *Dashboard* — admin console "not written yet" (README status); admin HTTP routes exist (`/v1/admin/*`)
  - ❌ Steamworks: *Admin console* — admin console not written (README status); all admin via `/v1/admin/*` REST + CLI
  - ❌ UGS Multiplayer: *Dashboard (Unity Cloud)* — No admin console (planned per README); everything via REST + Zanshin LiveOps integration.
  - 🟡 Colyseus: *Cloud API (beta, read-only): `GET /applications`, `/metrics/current` (CCU, rooms, CPU, mem, PM2 processes per instance,…* — Prometheus `/metrics` + OTel traces + `GET /v1/admin/cluster/*`; no deploy history/egress/billing API (n/a for self-hosted)
  - 🟡 Hiro/Satori: *Access control: teams, users, service users (API-only, Basic auth secret, CI/CD), permission layers org/title/resource…* — musubi operator keys with roles (admin/server/support/liveops/...), operator accounts + SSO/OIDC per parity.md, audit log; no team/title hierarchy or SCIM
  - 🟡 Nakama: *Access control: organizations (multiple owners), titles, teams, users, service users; layered permissions (org → title…* — musubi operator roles + multi-project + scoped keys + TOTP (`0090_operator_roles`, `0091_sso`, parity.md); no team/deny-layer model (rg -i "teams" admin → none)
  - 🟡 Rivet: *Org / project / namespace hierarchy, member invites, promote/demote/kick, billing settings* — operator accounts with roles + SSO (README L321) but one game per cluster, no org/project hierarchy
- **Already covered rows (3):** Nakama: Audit log: tamper-evident, org-level and per-deployment, categories (…; Nakama: Environment strategy: recommended dev / QA / load-test / prod deploym…; UGS Multiplayer: Regions / data residency

#### P2-2. Serverless / edge worker runtime mode  
*area:* deployment/ops · *musubi:* 🟡 partial (2h/4p/0m over 9 rows)
- **Competitors with it (6):** AccelByte, Colyseus, Edgegap, GameLift, Metaplay, Rivet
- **Why it matters:** Low-traffic titles want zero idle cost.
- **Implementation sketch (musubi terms):** P2 — not a fit for a stateful binary; document scale-to-zero node + `musubi dev` small footprint.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 GameLift: *Quickstart / roadmaps / design-backend guides* — README + docs/parity; no architecture guides
  - 🟡 Metaplay: *Database: SQLite locally, Aurora MySQL Serverless in cloud, MySQL 8; horizontal DB sharding by entity id (R8), reshardi…* — PostgreSQL only (sqlx migrations); no SQLite dev mode, no DB sharding (single PG + read scaling via replicas not documented)
  - 🟡 Rivet: *Local dev: `registry.start()` boots an embedded engine at `http://localhost:6420` with dashboard; `rivet dev` (`--provi…* — `musubi dev` (migrations, insecure defaults, WASM hot reload every 1 s, README L363); no bundled UI
  - 🟡 Rivet: *Troubleshooting pages: no runner connected, serverless health check failing, WS disconnects, actor stuck Pending* — `musubi doctor` checks config/DB/ports (`doctor.rs`); no troubleshooting docs
- **Already covered rows (2):** AccelByte: Extend Databases (serverless): NoSQL DocumentDB w/ TLS + TCP tunnel,…; Edgegap: OpenFaaS / serverless functions

#### P2-3. Ecosystem add-ons (ingress controller, image pre-pull, fleet GC, image syncer, GitHub Actions runners)  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (2h/3p/2m over 8 rows)
- **Competitors with it (5):** Agones/OM, Colyseus, Hathora, Metaplay, Nakama
- **Why it matters:** Operational conveniences.
- **Implementation sketch (musubi terms):** Add `fleet.ttl_secs` auto-delete + image pre-pull option in k8s/docker providers.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Agones/OM: *Octops Game Server Ingress Controller — automatic Ingress/DNS per GameServer* — No per-server ingress/DNS
  - 🟡 Agones/OM: *Octops Image Syncer — pre-pull game images on every node* — Firecracker warm pool + snapshot prepare (`POST /v1/server/fleets/providers/firecracker/prepare`, README:firecracker); nothing for docker/k8s image pre-pull
  - 🟡 Hathora: *CI examples (GitHub Actions, Jenkins)* — README documents CI for musubi itself; no deploy-from-CI recipe for game servers
  - 🟡 Metaplay: *CI/CD: `metaplay init ci` generates GitHub Actions/Bitbucket pipeline, machine users (`metaplay auth machine-login`), `…* — Dockerfile + Helm; no CI generator
- **Already covered rows (2):** Colyseus: Continuous deployment via GitHub Actions with `.colyseus-cloud.json`…; Nakama: CI/CD: service users (`<user>@<org>` HTTP Basic, secret shown once, n…

#### P2-4. Database backend options (Postgres, CockroachDB, SQLite dev, MongoDB)  
*area:* storage · *musubi:* ❌ missing (2h/2p/4m over 9 rows)
- **Competitors with it (3):** Beamable, brainCloud, Rivet
- **Why it matters:** Dev loop ergonomics and scale choices.
- **Implementation sketch (musubi terms):** Postgres-only. Add SQLite/PGlite-free dev mode is non-trivial; alternative: embedded Postgres via `musubi dev --embedded-pg`.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Rivet: *Per-actor embedded SQLite (`c.db`, raw SQL, transactions, named transactions)* — `rg -i sqlite` = 0 files; musubi storage is shared Postgres collections (`/v1/storage`, `musubi-storage`)
  - ❌ Rivet: *Drizzle ORM on actor SQLite, generated migrations, `onMigrate`* — no ORM/migration tooling for game data; musubi has schema TOML → derived storage/types (`musubi schema`, README "One TOML document") which is a different approach (have-ish for typed schema, missing for per-actor DB)
  - ❌ Rivet: *Actor Runtime Socket (beta): expose actor SQLite over Unix socket to external processes* — nothing comparable (`rg -i "unix socket\|uds"`)
  - ❌ Rivet: *Dashboard SQLite browser: schema, rows, execute SQL (`/inspector/database/*`)* — admin can query Postgres directly; no in-product data browser
  - 🟡 Beamable: *MicroStorage (MongoDB per service)* — Module host API storage collections (Postgres JSONB w/ query/aggregate) — no Mongo driver / arbitrary document DB
  - 🟡 brainCloud: *Mongo-style where queries, sort, hints, paging context* — `list_where` JSON field filters + `$where` ranges, cursor paging; not full Mongo operator set ($regex, $elemMatch…)
- **Already covered rows (2):** Beamable: Storage snapshot/restore/erase of local Mongo, `project open-mongo` (…; Rivet: Schema-driven typed storage + IDL/bindings generation

#### P2-5. Server browser query protocols (SQP/A2S) and server.json config  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (0h/1p/0m over 2 rows)
- **Competitors with it (2):** Steamworks, UGS Multiplayer
- **Why it matters:** Legacy community-server tooling.
- **Implementation sketch (musubi terms):** Add optional SQP/A2S responder in the DS SDK helper.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Steamworks: *Server browser / master server* — `GET /v1/matches?label&min_size&max_size&open_slots&region` and fleet server listing (`GET /v1/server/fleets/{id}/servers`) — no LAN discovery, no A2S, no friends/history filter, no "popularamongfriends"/`ServerFriends`…

#### P2-6. Resource footprint / scale limits / capacity docs (CCU per node, DB sizing)  
*area:* deployment/ops · *musubi:* 🟡 partial (0h/2p/0m over 2 rows)
- **Competitors with it (2):** AccelByte, Hathora
- **Why it matters:** Capacity planning.
- **Implementation sketch (musubi terms):** Exists: README "Measured". Add sizing guide.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 AccelByte: *AGS Observability: Grafana Cloud dashboards (Game Health: CCU by platform/region, login success %, matchmaking time, DS…* — Prometheus metrics + OTel exist (README "Observability") incl. matchmaker p50/p95/p99; no bundled Grafana dashboards or cost dashboards
  - 🟡 Hathora: *Capacity planning tools* — `musubi bench`/`soak` + `POST /v1/server/matchmaker/simulate` + fleet `demand` — no cost calculator

#### P2-7. Offline cache / local-first client data  
*area:* storage · *musubi:* ❌ missing (0h/0p/1m over 3 rows)
- **Competitors with it (2):** Metaplay, Photon
- **Why it matters:** Mobile players lose connectivity.
- **Implementation sketch (musubi terms):** SDK-side: offline cache in generated clients (depends on SDK work).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Metaplay: *Offline mode: full client runs with in-process `DefaultOfflineServer` (no network), handles built-in requests* — `musubi-cli dev` single process but still a server + Postgres

#### P2-8. Admin notes / internal comments on accounts  
*area:* accounts · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** AccelByte
- **Why it matters:** Support teams need shared context per player.
- **Implementation sketch (musubi terms):** Add `account_notes` table + `GET/POST /v1/admin/accounts/{id}/notes` (musubi-account).
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Admin internal comments on player accounts (10 latest, edit/delete own, ticket link, filter by admin)* — no admin notes on accounts

#### P2-9. Player profile (display name, avatar, metadata, language, timezone, location)  
*area:* accounts · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Pragma
- **Why it matters:** Baseline.
- **Implementation sketch (musubi terms):** Exists: `GET/PATCH /v1/account`, `GET /v1/users`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Pragma: *Account model: Social ID / Player ID / Personal ID / Provider ID* — single `account_id` (ULID) + linked identities; display_name but no discriminator (`rg -i discriminator` only in id.rs).

#### P2-10. GeoIP lookup / country enrichment  
*area:* analytics/observability · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** Edgegap
- **Why it matters:** Placement, compliance, analytics.
- **Implementation sketch (musubi terms):** Add optional MMDB lookup in musubi-edge when CDN headers are absent.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Edgegap: *IP lookup API* — `rg geoip\|maxmind\|ip_lookup` → none; only `cf-ipcountry` header pass-through

#### P2-11. Auth security events (suspicious login, critical event history, login history)  
*area:* auth · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** PlayFab
- **Why it matters:** Support needs a per-account security timeline.
- **Implementation sketch (musubi terms):** Exists: `suspicious_login` event + audit log. Add `GET /v1/admin/accounts/{id}/security-events` timeline.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 PlayFab: *Login telemetry: player_logged_in PlayStream event with IP, geo, platform; Players > Logins page lists login history* — musubi: login_attempts + sessions/devices list (`GET /v1/account/sessions`), audit log; geo lookup only via georouting country map, no per-login UI

#### P2-12. Device-code / QR remote login (TV, console, headless)  
*area:* auth · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** UGS LiveOps
- **Why it matters:** Consoles/TV and headless devices cannot type passwords.
- **Implementation sketch (musubi terms):** Add `/v1/auth/device_code/{start,poll,approve}` in musubi-auth with short codes + lease table.
- **What the reports found weaker/missing (merged rows):**
  - ❌ UGS LiveOps: *Code-Link (device-code flow for consoles/TV/VR: generate code, confirm from another device)* — `rg -i "device_code / code_link"` → 0; magic link is email-based only. Sketch: `POST /v1/auth/code/generate` → short code + poll endpoint, `POST /v1/auth/code/confirm` from a signed-in session, mint session for the wait…

#### P2-13. Operator SSO (OIDC + SAML) and admin IdP  
*area:* auth · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Pragma
- **Why it matters:** Studios want staff auth through corporate IdP.
- **Implementation sketch (musubi terms):** Exists: OIDC operator SSO (`/v1/admin/sso*`). Add SAML 2.0 SP (musubi-auth/saml.rs) for enterprise IdPs.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Pragma: *Operator login via identity provider* — operators use issued API keys (`POST /v1/admin/keys`); no SSO-backed operator identities.

#### P2-14. Container groups / sidecars / dependencies / per-instance groups  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (1):** GameLift
- **Why it matters:** Complex server topologies.
- **Implementation sketch (musubi terms):** Add `containers[]` (main + sidecars) in FleetSpec for k8s/docker providers.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 GameLift: *Container fleet overview (how it works)* — reconciler replaces dead servers (5 s loop) but no in-pod agent
- **Already covered rows (1):** GameLift: Modify / delete container groups

#### P2-15. Multi-cluster / multi-region / multi-supplier allocation with priorities and fallback  
*area:* dedicated servers/orchestration · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** GameLift
- **Why it matters:** Resilience and global coverage.
- **Implementation sketch (musubi terms):** Add `fleet_pools` with weighted members across providers/regions and fallback order (pools exist; extend with weights + cross-provider).
- **What the reports found weaker/missing (merged rows):**
  - 🟡 GameLift: *Multi-location fleets* — one fleet = one region; pools aggregate regions

#### P2-16. Per-player session creation limits and scale-down protection  
*area:* dedicated servers/orchestration · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** GameLift
- **Why it matters:** Abuse and safety during scale-in.
- **Implementation sketch (musubi terms):** Add `max_sessions_per_player_window` + `protected` flag on allocated servers in musubi-fleet.
- **What the reports found weaker/missing (merged rows):**
  - ❌ GameLift: *Resource creation limit policy* — `rg -i 'per_creator' or 'creation_limit'` → none (generic per-account rate limits only)

#### P2-17. Entitlements / DLC / platform ownership sync  
*area:* economy/IAP · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Steamworks
- **Why it matters:** Cross-store ownership.
- **Implementation sketch (musubi terms):** Exists: `/v1/entitlements*`, `/v1/server/entitlements*`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Steamworks: *Steam DLC / packages / autogrants* — entitlements synced from Steam DLC ownership; no key/package issuance (platform-side)

#### P2-18. Settlement / revenue reports / reconciliation feeds  
*area:* economy/IAP · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Steamworks
- **Why it matters:** Finance teams.
- **Implementation sketch (musubi terms):** Exists: `/v1/server/iap/steam/reconcile`. Add normalized settlements ledger + daily export.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Steamworks: *Live sales reporting* — Prometheus metrics + purchases ledger export; no revenue reporting dashboards (console not written)

#### P2-19. Group progression, XP, achievements, challenges, wars  
*area:* groups · *musubi:* ❌ missing (0h/1p/1m over 2 rows)
- **Competitors with it (1):** Hiro/Satori
- **Why it matters:** Guild meta-game.
- **Implementation sketch (musubi terms):** Exists: `/v1/server/groups/{id}/xp`, group challenges/wars. Gap: team achievements with member contributions, team public/private stats.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hiro/Satori: *Team achievements (shared goals, sub-achievements, preconditions, cron reset, auto_claim, total_reward, member contribu…* — musubi achievements are per account only (rg "group" achievements.rs -> none); group xp/level exists
  - 🟡 Hiro/Satori: *Stat events to publisher with old/new values; OCC conflict on concurrent team stat updates* — musubi `POST /v1/server/stats/update` + `GET /v1/server/stats/top`; stats change events not on bus (rg "stat" events.rs)

#### P2-20. Leaderboard metadata / versionId guard / archived versions / rank disable for huge boards  
*area:* leaderboards/tournaments · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** UGS LiveOps
- **Why it matters:** Season history and robust writes.
- **Implementation sketch (musubi terms):** Exists: `windows`. Add `expected_version` guard on submit and `ranks: false` mode for very large boards.
- **What the reports found weaker/missing (merged rows):**
  - ❌ UGS LiveOps: *`versionId` guard on score submission (stale clients can't post into a new season)* — No expected-window token on `POST /v1/leaderboards/{id}/records`; sketch: accept `window_id`, reject with 409 when it is not the live window

#### P2-21. Async placement object / placement events / queue of placements  
*area:* matchmaking · *musubi:* 🟡 partial (0h/2p/0m over 2 rows)
- **Competitors with it (1):** GameLift
- **Why it matters:** Scales allocation under load.
- **Implementation sketch (musubi terms):** Make `POST /v1/server/allocate` async-capable: `202 {placement_id}` + `GET /v1/server/session_placements/{id}` + webhook/WS event in musubi-fleet.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 GameLift: *Game session queues* — pools (fleets grouped by `pool`) + region routing (`allocate_routed`); no timeout/latency policy/priority/filter configuration object
  - 🟡 GameLift: *Game session queue object* — pool is a string attribute on fleets, not a configurable object

#### P2-22. Crossplay groups / platform pools / client version isolation  
*area:* matchmaking · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** EOS
- **Why it matters:** Platform policies and protocol compatibility.
- **Implementation sketch (musubi terms):** Exists: crossplay groups. Add `client_version` claim → pool key + `allowed_regions` per app.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 EOS: *Lobby member platform exposed (which platform each member is on) for crossplay rules* — member list has account ids; platform derivable from linked identity but not exposed in lobby payload (rg "platform" in lobbies.rs → none)

#### P2-23. Matchmaker controls: pause queue, rebalance, exclusion lists, kill switch, A/B rule variants  
*area:* matchmaking · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** AccelByte
- **Why it matters:** Live control during incidents.
- **Implementation sketch (musubi terms):** Add `enabled`/`paused` + `variants[{weight, rules}]` per queue with outcome tagging in matchmaker stats.
- **What the reports found weaker/missing (merged rows):**
  - ❌ AccelByte: *Match rebalance (post-match team rebalance API)* — no rebalance endpoint

#### P2-24. Matchmaking simulation / load testing of queues  
*area:* matchmaking · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** UGS Multiplayer
- **Why it matters:** Tune before launch.
- **Implementation sketch (musubi terms):** Exists: `POST /v1/server/matchmaker/simulate`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 UGS Multiplayer: *Admin API with dry run* — Queue CRUD at runtime; `simulate` dry-runs rules on virtual time (README) but config writes have no `dry_run` validation mode.

#### P2-25. Entity caps / per-client quotas / memory-pressure admission  
*area:* meshing/world · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Meshing vendors
- **Why it matters:** Protect the simulation.
- **Implementation sketch (musubi terms):** Exists: `soft_cap` per cell + spawn queue. Add world-wide hard cap, per-client owned-entity quota, memory-pressure refusal.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Meshing vendors: *Per-client entity caps* — per-client interest limited by `budget_bytes` (default 16 KB/tick) not entity count

#### P2-26. Network LOD / per-field quantization / per-entity replication rate  
*area:* meshing/world · *musubi:* 🟡 partial (1h/2p/1m over 4 rows)
- **Competitors with it (1):** Meshing vendors
- **Why it matters:** Bandwidth and CPU.
- **Implementation sketch (musubi terms):** Add per-component `rate_hz` + quantization hints in the IDL used by mesh ghost replication.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Meshing vendors: *Network LOD (archetypes)* — interest ranks entities by relevance×age and filters by component values, but no per-distance component/precision reduction (`rg lod` 1 hit)
  - 🟡 Meshing vendors: *Delta compression & quantization* — versioned per-component deltas + `pos_quantum` (0.01) for positions only; no per-field bit-width/range schema (`rg quantiz` 4 hits)
  - 🟡 Meshing vendors: *Schema & baking* — IDL (TOML → schemas/OpenAPI/Rust types) covers API/match state; world components are untyped JSON
- **Already covered rows (1):** Meshing vendors: Packet priority scheduling

#### P2-27. Non-spatial partitioning / offloading by entity type or layer  
*area:* meshing/world · *musubi:* 🟡 partial (2h/1p/1m over 4 rows)
- **Competitors with it (1):** Meshing vendors
- **Why it matters:** AI/physics offloading across workers.
- **Implementation sketch (musubi terms):** Add `layer` dimension to cell leases so a simulator type can own a component layer world-wide (musubi-mesh/controller.rs).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Meshing vendors: *Offloading (Actor Groups to dedicated workers)* — musubi partitions only by space (2-D cells); no non-spatial partition key (`lib.rs:8` "non-spatial partition keys plug into CellId later")
  - 🟡 Meshing vendors: *Load balancing strategies* — grid/quadtree with cost-based split/merge and handover hysteresis; no hex/overlap or non-spatial sharding
- **Already covered rows (2):** Meshing vendors: Multi-worker load-balancing strategies; Meshing vendors: Strategy Worker / Unified Server Interest / Working Sets

#### P2-28. Published scale benchmarks (CCU per shard/world)  
*area:* meshing/world · *musubi:* 🟡 partial (1h/1p/0m over 2 rows)
- **Competitors with it (1):** Meshing vendors
- **Why it matters:** Credibility with studios.
- **Implementation sketch (musubi terms):** Publish >2k-CCU single-world soak numbers (extend README "Measured").
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Meshing vendors: *Entity scale claims* — README benchmarks: 2 000 CCU per node, world soak 10k entities; no multi-10k CCU single-world demo
- **Already covered rows (1):** Meshing vendors: Dynamic server meshing

#### P2-29. Simulator / worker SDK (Rust sim SDK, Unity/Unreal GDK, streaming protocol)  
*area:* meshing/world · *musubi:* ❌ missing (1h/0p/2m over 4 rows)
- **Competitors with it (1):** Meshing vendors
- **Why it matters:** Engine-side integration.
- **Implementation sketch (musubi terms):** Exists: musubi-sim-sdk (Rust) + stream protocol. Gap: Unity/Unreal bindings.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Meshing vendors: *ECS GDK* — no Unity SDK
  - ❌ Meshing vendors: *Zoning vs offloading* — spatial only
- **Already covered rows (1):** Meshing vendors: Worker flags

#### P2-30. Typed commands / RPC between entities and workers (routing, ordering, deadlines)  
*area:* meshing/world · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Meshing vendors
- **Why it matters:** Cross-worker interactions.
- **Implementation sketch (musubi terms):** Exists: `interact` events. Add typed commands in IDL with response correlation and deadline.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Meshing vendors: *Component commands* — `interact` has seq ordering but no deadline/status semantics

#### P2-31. Minimum client version / API blocking / compatibility switchboard  
*area:* moderation/anti-cheat · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** brainCloud
- **Why it matters:** Force upgrades, kill broken clients.
- **Implementation sketch (musubi terms):** Add `[clients] min_version` per platform checked in musubi-edge + `/v1/admin/api_blocks`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *Security settings per app* — TLS at `musubi-edge`, lockouts, rate limits; **no minimum-client-version gate** (grep none)

#### P2-32. Voice moderation / voice evidence / child-voice detection  
*area:* moderation/anti-cheat · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** Photon
- **Why it matters:** Voice safety is a 2026 platform requirement.
- **Implementation sketch (musubi terms):** Depends on voice engine; hook Aurix moderation events into reports.

#### P2-33. Deterministic shared client/server game logic model (client-first execution, server re-validation)  
*area:* progression/LiveOps · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Metaplay
- **Why it matters:** Metaplay-style offline-tolerant meta games.
- **Implementation sketch (musubi terms):** Niche architecture; document WASM module reuse on client (wasm32) + journal of actions for re-validation.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Metaplay: *Client & server model listeners (`IPlayerModelClientListener`, server listener hooks), event-driven UI* — realtime `StorageUpdate`/status/notification pushes; no typed listener interface without SDK

#### P2-34. Discord bot / community integrations (roles as rewards, claimable rewards)  
*area:* progression/LiveOps · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** LootLocker
- **Why it matters:** Community-led growth.
- **Implementation sketch (musubi terms):** Add Discord webhook/bot adapter in musubi-integrations (role grant on entitlement, reward claim).
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Squire setup: token from console + `/setup [token]` in Discord; Channels & Roles (Redemption Role allowed to redeem key…* — No Discord bot

#### P2-35. Maintenance mode / developer-player flag / server time-skip / limited access playtests  
*area:* progression/LiveOps · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Metaplay
- **Why it matters:** Release-day and QA testing.
- **Implementation sketch (musubi terms):** Add `[ops] maintenance {enabled, allowlist, message}`, `accounts.developer` flag, `X-Musubi-Time-Offset` for dev, scheduled playtest windows gated by audience.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Metaplay: *System settings page: maintenance mode scheduling, client compatibility (logic version range, client redirect), time sk…* — config via TOML/env + `/v1/admin/config` (hot reload of some keys); no maintenance/time-skip

#### P2-36. Scheduled jobs API (named jobs, search, upcoming preview, state history, cancel/modify)  
*area:* progression/LiveOps · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** Beamable
- **Why it matters:** Operations visibility over timers.
- **Implementation sketch (musubi terms):** Exists: `/v1/admin/runtime/schedules*`, timers. Add job names, history, upcoming preview, modify.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Scheduler UI / Job Scheduler form in Portal* — No UI

#### P2-37. Surveys / custom signup fields / consent acknowledgements as player metadata  
*area:* progression/LiveOps · *musubi:* ❌ missing (0h/0p/2m over 2 rows)
- **Competitors with it (1):** LootLocker
- **Why it matters:** Research & compliance.
- **Implementation sketch (musubi terms):** Add `surveys` collection + response rows; signup fields via account metadata schema.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Playtests: hosted onboarding page (White Label signup), documents to sign (md/txt, require acknowledgement, statements)…* — rg -i 'playtest' = 0
  - ❌ LootLocker: *Custom Signup Fields on White Label signup (Markdown question, metadata key, type Text/Number/Checkbox/Select, Required…* — No signup form schema

#### P2-38. Spectators / replays / match recording  
*area:* realtime/matches · *musubi:* 🟡 partial (0h/2p/0m over 2 rows)
- **Competitors with it (1):** Rivet
- **Why it matters:** Esports & debugging.
- **Implementation sketch (musubi terms):** Exists: spectator mode, `/v1/matches/{id}/replay`.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Rivet: *Gateway API: `/gateway/{actor}/health\|action/{a}\|request/{path}\|queue/{q}\|metadata\|inspector/*` (state, connection…* — `GET /v1/server/matches/{id}`, replay/journal endpoints, `match_state` snapshots; no per-actor introspection API for connections/traces/db
  - 🟡 Rivet: *Versioning: history mismatch errors, replay from dashboard (`inspector/workflow/replay`), run history UI, step traces* — `GET /v1/server/workflows/{id}` + `musubi.workflows` SQL table; no history/replay UI

#### P2-39. Federation hooks (external inventory/NFT, external player init, challenge/solution wallet auth)  
*area:* runtime/extensibility · *musubi:* ❌ missing (1h/0p/1m over 2 rows)
- **Competitors with it (1):** Beamable
- **Why it matters:** Web3 or external-economy titles.
- **Implementation sketch (musubi terms):** P2 — `inventory_provider` trait with proxy reload; wallet-signature auth as OIDC-like provider.
- **What the reports found weaker/missing (merged rows):**
  - ❌ Beamable: *Web3 marketplace integrations: Sui, Polygon, Venly, Thirdwave Wallet Intelligence, Lamina1, Zebedee (Bitcoin), Halliday…* — No wallet auth or on-chain inventory federation
- **Already covered rows (1):** Beamable: Game-server hosting partners: Hathora, Edgegap, Agones (via `IFederat…

#### P2-40. Generic before/after hooks on every API & realtime message, disable API by hook  
*area:* runtime/extensibility · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Nakama
- **Why it matters:** Policy enforcement without forking.
- **Implementation sketch (musubi terms):** Exists: event-specific hooks (`before_rpc_*`, `before_chat_message`, …). Generalize: `before_<route_family>`/`after_<route_family>` host hooks for every ROUTES entry and realtime envelope.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Nakama: *Before/After hooks on every client API message (~85 pairs incl. authenticate/link/unlink per provider, storage, friends…* — musubi hooks are event-specific (`hook_authenticated`, `hook_auth_captcha`, `hook_suspicious_login`, `hook_leaderboard_reset`, `hook_tournament_end`, `hook_match_results`, `hook_matchmaker_matched`, `hook_mesh_place`, `…

#### P2-41. Typed RPC with generated client stubs and typed application errors  
*area:* runtime/extensibility · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** Pragma
- **Why it matters:** Compile-time safety across client/server.
- **Implementation sketch (musubi terms):** Exists: IDL RPC definitions + codegen for realtime types. Extend codegen to RPC stubs with typed error enums (musubi-idl).
- **What the reports found weaker/missing (merged rows):**
  - 🟡 Pragma: *SDK: typed RPC results & errors* — Unreal plugin returns JSON/`FMusubiError`.

#### P2-42. Followers (one-way follow graph)  
*area:* social · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** LootLocker
- **Why it matters:** Creators and UGC communities.
- **Implementation sketch (musubi terms):** Add `follows` table + `/v1/follow/{id}` (musubi-social); presence follow exists.
- **What the reports found weaker/missing (merged rows):**
  - ❌ LootLocker: *Followers (one-way follow, follower/following lists with cursor pagination, counts, probe-by-cursor check, blocked cann…* — Only presence `status_follow` (realtime subscription), no persistent follower graph/API (rg 'follower' in social = 0)

#### P2-43. Referral / invite codes with rewards  
*area:* social · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** Hiro/Satori
- **Why it matters:** Growth loop.
- **Implementation sketch (musubi terms):** Add `referral_codes` + `POST /v1/referrals/redeem` with reward tables (musubi-economy).
- **What the reports found weaker/missing (merged rows):**
  - ❌ Hiro/Satori: *Invite / referral codes with sender + recipient rewards* — rg -i "incentive\|referral\|invite_code" -> none (store coupons are operator codes, not player-generated referral codes with dual rewards)

#### P2-44. Social overlay / in-game UI layer  
*area:* social · *musubi:* ❌ missing (0h/0p/1m over 1 rows)
- **Competitors with it (1):** EOS
- **Why it matters:** Console-style UX without studio UI work.
- **Implementation sketch (musubi terms):** Out of scope for a backend; provide SDK UI widgets later.
- **What the reports found weaker/missing (merged rows):**
  - ❌ EOS: *EOS Overlay: in-game overlay (Windows D3D11/12/Vulkan; experimental D3D9/10/OpenGL; consoles with game-rendered UI) pro…* — musubi has no client overlay/UI of any kind (no SDK UI)

#### P2-45. Localization pipeline (strings per locale, OTA)  
*area:* storage · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** brainCloud
- **Why it matters:** Global launches.
- **Implementation sketch (musubi terms):** Add `localization` collection + `GET /v1/l10n/{lang}` with ETag; translation sheet import via CLI.
- **What the reports found weaker/missing (merged rows):**
  - 🟡 brainCloud: *Localization* — locales in push templates, account `lang`; no central language list / localized catalog strings

#### P2-46. Typed collections / schema-as-code / automatic migrations  
*area:* storage · *musubi:* 🟡 partial (0h/1p/0m over 1 rows)
- **Competitors with it (1):** SpacetimeDB
- **Why it matters:** Prevents bad data and gives typed clients.
- **Implementation sketch (musubi terms):** Exists: `musubi-idl` + `/v1/admin/storage_schemas*`. Gap: compatible auto-migration on schema change (SpacetimeDB).
- **What the reports found weaker/missing (merged rows):**
  - 🟡 SpacetimeDB: *Tables declared in module code, schema-as-code* — musubi IDL (`GET/PUT /v1/admin/idl`, `musubi-idl`, `musubi idl` CLI, Colyseus schema import) types realtime state; game data tables are fixed SQL migrations (122) — developers do not define arbitrary tables

### 3.4 Single-competitor gaps (681 items)

Features only one report listed (after normalization). Each line: priority · competitor · feature — competitor detail — *what the report found in musubi* — sketch. Sketch defaults to the owning crate/route family for the area.


**P1 · auth** — owner: musubi-auth (`/v1/auth/*`, `/v1/account/*`)

- ❌ **AccelByte** · Country API (country list, per-country age restriction) + Basic country groups (ISO3166-1 groups for pricing/legal) — *no country tables / country groups*
- ❌ **brainCloud** · Handoff / Set-top handoff codes — `CreateHandoffId` + `AuthenticateHandoff`; `CreateSettopHandoffCode` + `AuthenticateSettopHandoff` (TV/console device-code login) — *no device-code / handoff login flow in `musubi-auth` (grep `handoff` none)* — [source](https://docs.braincloudservers.com/api/capi/identity/createsettophandoffcode)
- ❌ **brainCloud** · Peer apps (shared services between apps) — `AttachPeerProfile`, `DetachPeer`, `GetPeerProfiles`, `RunPeerScript`, peer currencies; Peer Publish / Peer Services portal pages — *no peer-app model* — [source](https://docs.braincloudservers.com/api/capi/identity/attachpeerprofile)
- ❌ **brainCloud** · Timed status effects — `SetUserStatus`, `ExtendUserStatus[WithMax]`, `GetUserStatus`, `ClearUserStatus`; items can trigger statuses — *no timed status/buff primitive (grep `status effect, buff` → only fleet/sim)* — [source](https://docs.braincloudservers.com/api/capi/playerstate/setuserstatus)
- ❌ **Colyseus** · Client `client.auth.token` sent as `Authorization` header on all HTTP requests, stored in `localStorage`; `onChange` auth-state callback; `signOut()` — SDK — *no client SDK* — [source](https://docs.colyseus.io/auth/module)
- ❌ **Hiro/Satori** · Future-date testing (QA mode): bind identity's new sessions to a simulated date — Set via console; needs re-auth; reset — *rg -i "fake time\|time travel\|future" -> none (dev fixtures exist, no per-account clock override)* — [source](https://heroiclabs.com/docs/satori/concepts/segmentation/understand-player-identities/)
- ❌ **Hiro/Satori** · Identity detail view (events, audiences, experiments, flags, live events, messages tabs); exclude identity from audience — Console debugging per player — *no console; `GET /v1/admin/accounts/{id}` + audit/events bus only* — [source](https://heroiclabs.com/docs/satori/concepts/segmentation/understand-player-identities/)
- ❌ **PlayFab** · Player shared secret + encrypted logins (GetTitlePublicKey RSA, SetPlayerSecret, signed requests, API policy enforcement) — Must be enabled from title creation; RSA key encrypts 237 bytes; player secret set once per title — *No request-signing / per-player secret scheme; musubi relies on TLS + JWT + optional captcha on login (grep: no "player secret")* — [source](https://learn.microsoft.com/gaming/playfab/features/authentication/encryption/player-encryption-services)
- ❌ **Pragma** · Session baggage — Per-session key/value store (player & partner), social/game/service-targeted variants, per-entry and total limits (`BaggageEntryLimitExceed… — *sessions carry fixed fields (device, platform, metadata on account) — no arbitrary per-session baggage propagated to logs/metrics.* — [source](https://pragma.gg/docs/2026.3.0/concepts/authentication/session-baggage)
- ❌ **SpacetimeDB** · Module HTTP handlers (beta) — Router with get/head/options/put/delete/post/patch/any, nest/merge, strict routing, served at `ANY /v1/database/{db}/route/*`; `Authorizati… — *modules cannot expose custom HTTP routes; musubi exposes fixed REST + `/v1/rpc`*
- ❌ **SpacetimeDB** · Views (derived, subscribable read models) — Read-only, auto-updating; procedural views (indexed lookups only, no scans) vs query-builder views (filters, semijoins, incremental eval);… — *no server-defined derived/subscribable read model; nearest is `StorageSubscribe` on raw collections*
- ❌ **Steamworks** · Browser login via OpenID 2.0 — steamcommunity.com/openid, returns claimed_id with SteamID; used for websites & web MTX — *musubi has no Steam OpenID relying-party flow; web auth is via its own email/magic-link/passkey/OIDC (`POST /v1/auth/oidc/{provider}`) — Steam only via client…* — [source](https://partner.steamgames.com/doc/features/auth)
- ❌ **Steamworks** · Encrypted application tickets — per-title symmetric key; `RequestEncryptedAppTicket(user data)` → verify offline with `sdkencryptedappticket` lib (`BDecryptTicket`, `BIsTi… — *no encrypted-app-ticket decryption path; `rg -i "EncryptedAppTicket\|encrypted_app"` → 0 hits* — [source](https://partner.steamgames.com/doc/features/auth)
- ❌ **Steamworks** · Free-weekend / timed-trial awareness — `BIsSubscribedFromFreeWeekend`, `BIsTimedTrial(secs allowed/played)`, `TimedTrialStatus_t` — *no license-type metadata in entitlements* — [source](https://partner.steamgames.com/doc/api/ISteamApps)

**P1 · accounts** — owner: musubi-account (`/v1/account`, `/v1/admin/accounts/*`)

- ❌ **Hathora** · Egress accounting per process — Total + rate egress per process; billed per GB (cloud $0.12/GB 2024; $0.09/GB 2025 on-demand, $0 reserved) — *`rg egress` → none* — [source](https://blog.hathora.dev/changelogs-november-2023/)
- ❌ **Hiro/Satori** · Deterministic client system startup order + custom client systems framework (Unity) — Client architecture — *no client framework* — [source](https://heroiclabs.com/docs/hiro/unity/getting-started/custom-systems/)
- ❌ **PlayFab** · GetUserPublisherData / Internal / ReadOnly variants (shared across titles of a publisher) — Server API — *No publisher scope* — [source](https://learn.microsoft.com/gaming/playfab/features/config/titledata/using-publisher-data)
- ❌ **PlayFab** · Publisher Data & User Publisher Data (read-only/internal variants) shared across titles in a studio — Server APIs only for writes — *No publisher/studio-level data layer across projects* — [source](https://learn.microsoft.com/gaming/playfab/features/config/titledata/using-publisher-data)
- ❌ **Rivet** · Transactions: `c.db.transaction(fn)` named/anonymous, nested disallowed; `c.db.exec/select/get` — docs — *no actor-local DB; `/v1/storage` batch writes are atomic per request (`musubi-storage`)* — [source](https://rivet.dev/actors/docs/sqlite/)

**P1 · storage** — owner: musubi-storage (`/v1/storage/*`, IDL collections)

- ❌ **Beamable** · Content tags & tag-based `ContentQuery`; `OmitContentManifestTags` — `content tag add/rm/set`; tag filter in events/Portal — *No tags on content entries* — [source](https://docs.beamable.com/docs/cli-content)
- ❌ **brainCloud** · Global (app) Statistics — `IncrementGlobalStats`, `ProcessStatistics`, `ReadAllGlobalStats`, `ReadGlobalStatsForCategory/Subset`; portal Global Statistics page — *no app-wide counters (grep `global_stat` none); storage aggregate is query-time only* — [source](https://docs.braincloudservers.com/api/capi/globalgamestatistics)
- ❌ **brainCloud** · Server time — `ReadServerTime`; `/servertime` endpoint (5.4) — *no `/v1/time` route (trivial)* — [source](https://docs.braincloudservers.com/api/capi/time)
- ❌ **Metaplay** · Sources: Google Sheets (default), CSV, JSON, custom sources; custom `ConfigParser` for types (rewards etc.), collection syntax, nested data — *no spreadsheet/CSV ingestion pipeline; JSON only via `musubi export/import`* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/deep-dive-custom-game-config-build-pipelines)
- ❌ **Photon** · Input delay (2.1) — cap predicted ticks to cut resim CPU; NetworkProjectConfig Input Delay Settings — *no input_delay concept (rg=0)* — [source](https://doc.photonengine.com/fusion/current/manual/advanced/time-synchronization)
- ❌ **Rivet** · `options.crashPolicy` restart / sleep / destroy — per actor — *fixed failover behaviour (`musubi-actor`); not configurable per module* — [source](https://rivet.dev/actors/docs/actor-configuration/)
- ❌ **Steamworks** · Network simulation / debug config — config values for fake packet loss, lag, reorder, debug output levels — *`rg -i "fake_loss\|packet_loss\|latency_sim"` → 0; `musubi soak` chaos only kills nodes* — [source](https://partner.steamgames.com/doc/api/ISteamNetworkingUtils)
- ❌ **UGS LiveOps** · Override priority — Low/Medium/High or numeric 1–1000; first-created wins ties — *No layered config resolution in core (one document per config name) — sketch: `configs.<name>.overrides[]` with audience + priority merged server-side in `GET…* — [source](https://docs.unity.com/en-us/remote-config/game-overrides-and-settings)

**P1 · social** — owner: musubi-social (`/v1/friends*`, `/v1/notifications*`, realtime `status_*`/`party_*`)

- ✅ **brainCloud** · Async turn-based matches — `CreateMatch[WithInitialTurn]`, `SubmitTurn(version, matchState, statistics, pushContent, summary)`, `UpdateMatchSummaryData`, `UpdateMatch… — *no turn-based async match service (grep `turn` none); would be modelled over storage + notifications* — [source](https://docs.braincloudservers.com/api/capi/asyncmatch)
- ❌ **Nakama** · Friend codes guide (runtime-generated short codes via storage index + RPC) — pattern, not built-in — OSS guide — *no friend-code primitive in musubi either (rg -i friend_code → none); would be a WASM module pattern as in Nakama* — [source](https://heroiclabs.com/docs/nakama/guides/concepts/friend-codes/)
- ❌ **SpacetimeDB** · Submodules (TS only) — Namespaced tables/functions/views/scheduled tables/HTTP handlers; lifecycle reducers root-only; reusable library (cron, crypto, rate-limit,… — *musubi modules are separate WASM units with host API; no composable module namespaces/library ecosystem*

**P1 · chat** — owner: musubi-chat (`/v1/channel/*`, realtime `channel_*`)

- ❌ **Photon** · Audio processing API — hooks for DSP, lip sync, speech recognition on incoming/outgoing frames — *no evidence recorded* — [source](https://doc.photonengine.com/voice/current/getting-started/voice-intro)
- ❌ **Photon** · Bandwidth optimisation — VAD, interest groups, bitrate/frame tuning docs — *no evidence recorded* — [source](https://doc.photonengine.com/voice/current/reference/optimization)
- ❌ **Photon** · Platforms — Windows, UWP, macOS, Linux, Android, iOS, visionOS, WebGL (no Safari), PS4/PS5/Switch/Xbox (add-ons), MagicLeap, HoloLens 2 — *no evidence recorded* — [source](https://doc.photonengine.com/voice/current/getting-started/voice-intro)
- ❌ **Photon** · Speaker / playback — jitter buffer (PlayDelay), Unity AudioSource 3D spatialization, FMOD speaker, audio processing API (DSP, lip-sync, speech recognition hooks) — *no evidence recorded* — [source](https://doc.photonengine.com/voice/current/getting-started/voice-intro)
- ❌ **Photon** · Speaker PlayDelay / jitter buffer — configurable playback delay to absorb jitter — *no evidence recorded* — [source](https://doc.photonengine.com/voice/current/getting-started/voice-intro)
- ❌ **UGS Multiplayer** · Couch co-op multi-login — multiple signed-in users with independent devices (PS4/Xbox/Windows) — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/couch-co-op/couch-co-op-overview)
- ❌ **UGS Multiplayer** · Echo channels (mic test) — `-e-` — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/channels/channel-types)
- ❌ **UGS Multiplayer** · Large 3D channels (≤2000 participants) — enterprise-only; disables text and display names in 3D — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/channels/large-3d-channel-setting)
- ❌ **UGS Multiplayer** · Muting matrix — local mic/speaker; user→user per channel or all channels; cross-mute (bidirectional via block lists at login); mute whole channel; moderato… — *Text-chat mute/ban exists (`chat` moderation); no voice.* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/muting)
- ❌ **UGS Multiplayer** · Platforms — Windows, macOS, Linux x64/arm64, Android API14+, iOS 10.3+, WebGL, visionOS (16.12), consoles via approved portal; Unity, Unreal, Core C SD… — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/supported-platforms)
- ❌ **UGS Multiplayer** · Real-time recording — OGG 15 s chunks + metadata streamed over WSS to your server; TAM-enabled — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/real-time-recording/real-time-recording)
- ❌ **UGS Multiplayer** · S2S login API (`/api2/viv_signin.php`), fully-qualified identifiers, error handling — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/server-to-server-api-reference/login-api)
- ❌ **UGS Multiplayer** · Session groups / multichannel — several channels at once; transmission modes tx-all / tx-single / tx-none; common codec — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/channels/session-groups)
- ❌ **UGS Multiplayer** · Text-to-speech — US English only, one user; destinations local/remote/both × simple/queue/replace; raw audio access — *no evidence recorded* — [source](https://docs.unity.com/en-us/vivox-core/developer-guide/text-to-speech)

**P1 · leaderboards/tournaments** — owner: musubi-leaderboard (`/v1/leaderboards/*`, `/v1/tournaments/*`, `/v1/server/stats/*`)

- ❌ **Beamable** · Events calendar view in Portal (recurring events, tag filter) — Portal 1.5/1.6 — *No UI* — [source](https://github.com/beamable/Changelogs/blob/main/com-beamable-portal-changelog.md)
- ❌ **Hiro/Satori** · Time-based vs score-based events — score_target, score_target_players, score_time_limit_sec, max_rolls, roll_cooldown_sec (per reward tier override), phases via reset_schedul… — *no race-to-target cohort completion or multi-round rolls in musubi leaderboards/tournaments* — [source](https://heroiclabs.com/docs/hiro/concepts/event-leaderboards/)

**P1 · matchmaking** — owner: musubi-matchmaker (`[[matchmaker.queues]]`, `/v1/server/matchmaker/*`, `/v1/lobbies/*`)

- ❌ **Edgegap** · Automated ping in SDKs — Unity SDK + Unreal EGIK measure beacons and attach latencies to tickets — *No client SDK does this (client must send `ping_<region>` itself)* — [source](https://docs.edgegap.com/learn/orchestration/ping-beacons)
- ❌ **Edgegap** · CORS allowlist for WebGL clients — Exact + wildcard origins — *`rg -i cors` → none in server/edge* — [source](https://docs.edgegap.com/learn/matchmaking/matchmaker-in-depth)
- ❌ **Edgegap** · Session bulk stop — `POST /v1/sessions/bulk-stop` with filters — *no evidence recorded* — [source](https://api.edgegap.com/swagger.json)
- ❌ **GameLift** · Explicit sort example — `absoluteSort`/`distanceSort` to pre-sort batches — *no evidence recorded* — [source](https://docs.aws.amazon.com/gamelift/latest/flexmatchguide/match-examples-4.html)
- ❌ **GameLift** · Property expression language — `teams[red].players.attributes[skill]`, aggregations min/max/avg/median/sum/count/stddev/flatten/set_intersection on nested lists — *`rg -i 'stddev' or 'set_intersection' crates/musubi-matchmaker` → none* — [source](https://docs.aws.amazon.com/gamelift/latest/flexmatchguide/match-rules-reference-property-expression.html)
- ❌ **PlayFab** · Lobby & Matchmaking client SDK (C/C++ PlayFabMultiplayer, Unity plugin, Unreal via OSS): async state-change processing, operation queuing, thread affinity control, retry behaviour (exp backoff 2s→20s, 20s window), logging, platform notes (Android/Switch/PlayStation), release notes — Approval needed for console SDKs — *No client SDK (README: SDKs not written; only IDL-generated stubs via `musubi idl`)* — [source](https://learn.microsoft.com/gaming/playfab/features/multiplayer/lobby/lobby-matchmaking-sdks/lobby-matchmaking-sdks)
- ❌ **UGS Multiplayer** · Bulk update (service account only) — one call: lobby update + player updates + add/remove players, `ignoreIneffectualUpdates` — *Server key can call individual routes; no transactional bulk lobby mutation.* — [source](https://docs.unity.com/en-us/oas-lobby/1.0.0)
- ❌ **UGS Multiplayer** · Cloud-mediated client-authoritative sessions (CMB) — `POST /v1/sessions {lobbyId, region}`; authority spread across clients; lightweight state service; session owner (first client) runs scenes… — *No client-authority object-ownership relay; musubi offers server-authoritative WASM matches or plain relay.* — [source](https://docs.unity.com/en-us/oas-distributed-authority/1.0.0)
- ❌ **UGS Multiplayer** · Netcode for Entities integration — auto client/server worlds (MPS 1.2), `EntitiesDriverConstructor`, N4E 7 namespace support (2.3.2) — *No N4E driver.* — [source](https://docs.unity.com/en-us/mps-sdk/build-with-netcode-for-entities)
- ❌ **UGS Multiplayer** · Session viewer / view sessions (dashboard) — browse live sessions in Unity Dashboard — *No admin console (README: "admin console not written"); `GET /v1/admin/*` lobby listing only via API.* — [source](https://docs.unity.com/en-us/mps-sdk/session-viewer)

**P1 · realtime/matches** — owner: musubi-actor + musubi-transport (realtime `match_*` envelopes, `/v1/matches/*`)

- ❌ **Meshing vendors (Improbable SpatialOS)** · Entity-component-worker model, layers — components grouped into layers; worker type = one attribute (e.g. `physics`); per-layer load balancing; client-workers outside layers — *single simulator kind per world; no component layers simulated by different worker types* — [source](https://documentation.improbable.io/spatialos-overview/docs/layers)
- ❌ **Meshing vendors (coherence)** · Global Query / Is Global — entities marked global visible to any client with a GlobalQuery — *no "always relevant" flag; everything is radius-bound (`rg -i global` in mesh: none)* — [source](https://docs.coherence.io/manual/optimization/areas-of-interest)
- ❌ **Meshing vendors (Improbable SpatialOS)** · Launcher / share links — launch GDK clients against a deployment, share links; Windows/mac only — *no build distribution* — [source](https://documentation.improbable.io/spatialos-tools/docs/launcher)
- ❌ **Photon** · Camera selection, preview rotation, custom shaders — device enumeration, orientation handling, shader-based rendering of streams — *no evidence recorded* — [source](https://doc.photonengine.com/video/current/manual/camera-selection)
- ❌ **Photon** · SuppressRoomEvents / SuppressPlayerInfo (0x40) — skip Join/Leave events, actor list and actor props in join response to cut bandwidth in large rooms — *presence events always sent in relay matches; no suppress flag* — [source](https://doc.photonengine.com/server/v5/version-history/photon-5-0-12-release)
- ❌ **Photon** · Verified vs predicted frames — Verified (all inputs confirmed), Predicted/PredictedPrevious/PreviousUpdatePredicted; Frame API — *no evidence recorded* — [source](https://doc.photonengine.com/quantum/current/manual/frames)

**P1 · dedicated servers/orchestration** — owner: musubi-fleet (`FleetSpec`, `/v1/server/fleets/*`, `/v1/gameserver/*`, providers)

- ❌ **Agones/OM** · GameServer creation/deletion throttles: `maxCreationParallelism`, `maxGameServerCreationsPerBatch`, `maxDeletionParallelism`, `maxGameServerDeletionsPerBatch`, `maxPodPendingCount` — Helm values — *Reconciler sizes the buffer every 5 s with no configurable batch limits*
- ❌ **Agones/OM** · Policy `Chain`: ordered list of `{id, type Schedule/Webhook/Buffer…}` evaluated by priority with fallback — Events logged with chain id — *Single policy per fleet*
- ❌ **Agones/OM** · Policy `Wasm` (alpha 1.54, Extism): module from `service`/`url` + `caBundle`, sha256 pin, `function` (default `scale`), `config` map; same JSON contract as webhook — Runs in the controller, no extra service — *musubi has a WASM module runtime (`musubi-runtime`) but it exposes no fleet-scaling hook (`rg -i 'scale' crates/musubi-runtime` → no autoscale hook)*
- ❌ **Agones/OM** · Stable network identity: headless Service + `subdomain` gives each GameServer a DNS name — For server-to-server addressing — *Servers addressed by IP:port only*
- ❌ **Agones/OM** · `allocationOverflow.labels/annotations` applied to Allocated servers that exceed desired replicas after scale-down — So the game can wind down gracefully — *Scale-down ignores allocated servers (they finish and exit); no marker is applied*
- ❌ **Agones/OM** · `spec.lists{key:{capacity,values[]}}` (string lists, capacity ≤ `gameservers.lists.maxItems`, default 1000) — Used for connected-player IDs, room IDs — *No per-server list state; labels JSON (`POST /v1/gameserver/label`) is free-form but not capacity-tracked, not filterable at allocation*
- ❌ **Colyseus** · `COLYSEUS_LATENCY=<ms>` env + `server.simulateLatency(ms)` (dev only, must not run in prod) — 0.18.7 — *`rg -i 'simulate_latency\|latency_ms' crates` → only bench/soak tooling, no server-side artificial latency switch* — [source](https://github.com/colyseus/colyseus/blob/master/bundles/colyseus/CHANGELOG.md)
- ❌ **Colyseus** · `server.simulateLatency(ms)` dev helper — *no latency injection switch (bench/soak only)* — [source](https://docs.colyseus.io/server)
- ❌ **Edgegap** · Deployment map + balance-point heatmap — Dashboard map of live deployments; heatmap of request centroids per version — *No UI; `GET /v1/server/fleets/{id}/demand` gives demand history as JSON only* — [source](https://docs.edgegap.com/learn/orchestration/deployments)
- ❌ **Edgegap** · Image verification — Version flag `verify_image` validates pullability at save time — *No image pull validation on `fleet push`* — [source](https://api.edgegap.com/swagger.json)
- ❌ **Edgegap** · S3 Endpoint Storage — Register bucket (name, key, encrypted secret, endpoint URL, bucket); test upload; Linode/AWS specifics — *`rg -i s3` → none* — [source](https://docs.edgegap.com/docs/endpoint-storage)
- ❌ **GameLift** · Anywhere fleet cost attribute — `AnywhereConfiguration.Cost` (per-hour cost string) used by queue COST priority — *no cost attribute on fleets* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_AnywhereConfiguration.html)
- ❌ **GameLift** · Build resources — `CreateBuild` from S3 or `RequestUploadCredentials` zip upload (CLI `upload-build`), OS, version, status INITIALIZED/READY/FAILED, size; `U… — *no artifact store: builds are specs referencing an image/command (`FleetBuild.spec`), no upload of binaries* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-build-intro.html)
- ❌ **GameLift** · GameLift Agent — Java 17 open-source agent: registers compute, websocket to service, launches processes per runtime config, heartbeats — *no agent for BYO hosts (ssh provider pushes processes; no pull-based agent)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-dev-iteration-agent.html)
- ❌ **GameLift** · Install script — `install.sh` / `install.bat` executed at fleet creation; events on success/failure — *no install hook; Dockerfile responsibility* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-build-packaging.html)
- ❌ **LootLocker** · Player Files server-side: list/get/delete by id or key, upsert by key, file revisions (list, get, promote revision to current) — *No per-player file store with revisions (storage values are JSON ≤ configurable size; UGC blobs single-version)* — [source](https://ref.lootlocker.com/server)
- ❌ **Steamworks** · Server reputation / gameplay stats — `GetServerReputation`, `GetGameplayStats` (GSReputation_t, GSGameplayStats_t) — *no server reputation score* — [source](https://partner.steamgames.com/doc/api/ISteamGameServer)
- ❌ **UGS Multiplayer** · API: reservations reserve/unreserve on machine — *no evidence recorded* — [source](https://support.multiplay.dev/articles/2944878934-reserve-a-server-on-a-machine)
- ❌ **UGS Multiplayer** · Differential (delta) image updates + mirror network — difference analysis accept/reject, primary + delta archives, regional mirror network — *Container images / binaries pulled by provider; no delta image pipeline.* — [source](https://support.multiplay.dev/articles/4951324689-differential-updates)
- ❌ **UGS Multiplayer** · Reservations (server-browser model) — server marks itself reserved retroactively; reservation fleets have one active build config with graceful switch + timeout — *Allocation-only model; `GET /v1/matches?label=&open_slots=` gives a server browser for authoritative matches, not for fleet servers.* — [source](https://support.multiplay.dev/articles/8698893629-reservations)

**P1 · meshing/world** — owner: musubi-mesh + musubi-sim-sdk (`/v1/server/worlds/{w}/*`, realtime `world_*`)

- ❌ **Meshing vendors (coherence)** · 3-D space — Unity Vector3 positions, 3-D LiveQuery cubes — *`musubi-mesh/src/lib.rs:8` "Space is 2D here (x, y); 3D/octree ... later"* — [source](https://docs.coherence.io/manual/components/coherence-live-query)
- ❌ **Meshing vendors (Hadean)** · Networked physics — "Future release: Networked Physics — distributed physics solution" — *physics explicitly out of core (user decision); neither shipped* — [source](https://web.archive.org/web/20200215103741/https://www.hadean.com/aether-engine/features)
- ❌ **Meshing vendors (Hadean)** · Portal — create/find platform-clusters, personal accounts — *no console UI* — [source](https://hadean.com/blog/the-hadean-platform-sdk-what-the-heck-did-we-just-make/)
- ❌ **Rivet** · Actor per entity / coordinator & data actor patterns — One actor per user/document/room; coordinator actors index data actors; sharding, fan-out — *no equivalent; musubi models entities as rows in Postgres subsystems (accounts, groups, chat channels) rather than addressable compute units — `rg -i "coordina…* — [source](https://rivet.dev/actors/docs/design-patterns/)

**P1 · economy/IAP** — owner: musubi-economy (`/v1/store*`, `/v1/iap/*`, `/v1/inventory*`, `/v1/server/rewards/*`)

- ❌ **Beamable** · VIP tiers (`VipContent`) — Content type for VIP levels — *No VIP tier system (grep vip hits are unrelated)* — [source](https://docs.beamable.com/docs/content-overview)
- ❌ **Metaplay** · Item references `MetaRef`/`MetaConfigId`, cross/recursive references, late resolution, reference analyzer, item aliases/renaming without data migration — *no typed reference graph in content documents* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/game-config-item-references)
- ❌ **Hiro/Satori** · Donations (ask friends/team for items) — Donation defs: cost, max_count, duration_sec, user_contribution_max_count, recipient_reward, contributor_reward; per-user storage; RPCs req… — *rg -i donation in musubi -> none (group treasury/gifts exist but no player-to-player request/contribute flow with dual rewards)* — [source](https://heroiclabs.com/docs/hiro/concepts/economy/donations/)
- ❌ **Hiro/Satori** · Offline sync (BASE_SYNC, device prefs) — client stores offline changes and syncs; `ForceNetworkProbe`; offline mode for Unity — *no offline-first client reconciliation (no SDK)* — [source](https://github.com/heroiclabs/hiro/blob/master/hiro-openapi.yml)
- ❌ **SpacetimeDB** · CLI live subscribe — `spacetime subscribe DB QUERY -n -t --print-initial-update` — *no CLI tail of realtime data*
- ❌ **SpacetimeDB** · Client-side cache — SDK keeps consistent local mirror; reads free; row callbacks `on_insert/on_update/on_delete` (+`on_before_delete`); unique-index seek; no B… — *no client SDK with local state mirror (SDKs "not written yet", README)*
- ❌ **SpacetimeDB** · Typed query builder (client) — `where/filter`, eq/ne/lt/lte/gt/gte, and/or/not, left/right semijoin on indexed columns; TS, C#, Rust, Unreal (C++ + Blueprint) (REL 1.11/1… — *no client query language*
- ❌ **Steamworks** · Dependencies & collections — `AddDependency/RemoveDependency`, `AddAppDependency`, collections (`GetCollectionDetails`), children in queries — *no UGC dependency graph (`rg -i "ugc.*depend"` → 0)* — [source](https://partner.steamgames.com/doc/api/ISteamUGC)
- ❌ **Steamworks** · Inspect item link — `InspectItem(link)` for items seen in market/profiles — *no inspect/share links* — [source](https://partner.steamgames.com/doc/api/ISteamInventory)
- ❌ **UGS Multiplayer** · Relay via `NetworkDriverStore` — custom `INetworkStreamDriverConstructor` before/after world creation — *no evidence recorded* — [source](https://docs.unity3d.com/Packages/com.unity.netcode@7.1/manual/networking/using-relay.html)

**P1 · progression/LiveOps** — owner: musubi-economy progression/quests/seasons + `/v1/server/audiences`, `/v1/liveops/*` (Zanshin)

- ❌ **EOS** · NAT P2P Interface: ICE-style connection (local → STUN direct → TURN relay), socket-id keyed connections, accept/close, notifications (request/established/interrupted/closed), relay control modes (AllowRelays/NoRelays/ForceRelays to hide IPs), NAT type query/cache, port range (default 7777 + 99), packet queue sizes, channels (0–255), reliable/unreliable packet modes, encrypted traffic, UTC clock requirement; no Epic account needed — Free relay (TURN) infrastructure hosted by Epic; max packet ~1170 bytes — *musubi has no client↔client P2P/NAT traversal or relay servers; relayed matches route through node over WS/QUIC/WebRTC-DC (server-mediated), lobbies can carry…* — [source](https://dev.epicgames.com/docs/epic-online-services/multiplayer/nat-p2p-interface/p2p-reference)
- ❌ **LootLocker** · Hero Classes: specialised sub-class with own loadout and Can Equip / Can't Equip exceptions; in-game create/update/list characters & heroes, get own/other players' loadout, equip/unequip to default or specific class, equippable contexts — *rg -i 'hero\|loadout\|character class' = 0* — [source](https://docs.lootlocker.com/game-systems/classes-and-heroes/how-to/hero-classes)
- ❌ **Metaplay** · Client integration: `GotLiveOpsEventUpdate` listener, interactive reward claiming (`AllowRemove`, `OnLatestUpdateAcknowledged`, `PlayerClearLiveOpsEventUpdates`), event-start popups, remaining-time from `ScheduleMaybe`, force-activate events in development — *no client SDK events* — [source](https://docs.metaplay.io/feature-cookbooks/in-game-events/advanced-liveops-events-integration)
- ❌ **Hiro/Satori** · Computed properties per event: <event>Count/SeenFirst/SeenLast/ValueFirst/ValueHigh/ValueLast/ValueLow/ValueSum; last active session id — Auto-generated for every taxonomy event — *no event-derived aggregates per player (stats are explicit server writes)* — [source](https://heroiclabs.com/docs/satori/concepts/segmentation/understand-player-properties/)
- ❌ **Hiro/Satori** · Tutorial sequences with step tracking — Multiple tutorials with start_step/max_step; accept/decline/abandon/update/reset; lifecycle hooks for server-side rewards — *rg -i tutorial -> none (can be emulated with storage objects but no system)* — [source](https://heroiclabs.com/docs/hiro/concepts/tutorials/)
- ❌ **Pragma** · `DataRightsServiceConfig` / `DataRightsPollingServiceConfig` — `includeGameDataReports`, `checkRequestsFrequencyInMinutes` (min 1, default 10). — *synchronous export, no polling job.* — [source](https://pragma.gg/docs/2026.3.0/concepts/accounts/privacy-rights)
- ❌ **Rivet** · Serverless workers invoked by the control plane over HTTP (`/api/rivet/start`), request held open; `requestLifespan`, `drainGracePeriod` 1,800 s — Vercel (295 s Hobby / 3,595 s Pro), Lambda 15 min, Cloud Run, Supabase, Cloudflare, Freestyle — *`rg -i serverless` = 0; musubi runs WASM inside its own nodes only* — [source](https://rivet.dev/docs/deploy/self-host/workers/)
- ❌ **UGS Multiplayer** · Ownership transfer / `OwnershipRequestStatus`, `IsSessionOwner` — NGO distributed-authority API — *no evidence recorded* — [source](https://docs.unity3d.com/Packages/com.unity.netcode.gameobjects@3.1/manual/terms-concepts/distributed-authority.html)

**P1 · moderation/anti-cheat** — owner: musubi-server bans.rs/anticheat.rs/automod.rs (`/v1/admin/accounts/{id}/ban`, `/v1/reports`)

- ❌ **Metaplay** · Security bulletins (MET-2024-001 … MET-2026-002, e.g. SharpCompress CVE-2026-44788) with severity and affected SDK versions — *no `SECURITY.md`/advisory process in repo* — [source](https://docs.metaplay.io/miscellaneous/security-bulletins/index)
- ❌ **UGS Multiplayer** · Screening results — per-player tracks + waveform, join/leave/mute events, characteristics (feminine/masculine/unknown/**child**), languages w/ ratio (50+ codes… — *no evidence recorded* — [source](https://docs.unity.com/en-us/oas-safevoice/1.0.0)

**P1 · analytics/observability** — owner: musubi-server metrics.rs/telemetry.rs/events.rs + musubi-integrations sinks

- ❌ **Beamable** · `platform_ua_attribution` user-acquisition events — UA campaign attribution — *No attribution ingestion* — [source](https://docs.beamable.com/docs/analytics-overview)
- ❌ **brainCloud** · Report service (analytics reports via API) — `SysGetLatestReport`, `SysGetReport`, `SysGetReportForStartDate`, `SysListLatestReports` (≤20) — *analytics delegated to Zanshin; no report API* — [source](https://docs.braincloudservers.com/api/capi/report)
- ❌ **GameLift** · Console dashboard — Account-wide overview — *no UI* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-console-dashboard.html)
- ❌ **GameLift** · Console dashboards — Fleet metrics tabs, game & player session metrics, dashboard page — *README: admin console UI not written (API only)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-console-dashboard.html)
- ❌ **GameLift** · Console fleets page — Fleets list, display preferences saved per user, clone fleet, create workflow — *no UI* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-console-fleets.html)
- ❌ **GameLift** · Console queues page — Queue list, destinations, metrics — *no UI* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/queues-console.html)
- ❌ **Hathora** · Download full logs for stopped process — `DownloadLogForProcess`; Console download; retained 72 h — *as above* — [source](https://hathora.dev/api (swagger.json, archived))
- ❌ **Hathora** · Per-process logs stream — `GetLogsForProcess` with `follow` (SSE-like stream) and `tailLines` 1..5000 — *`rg stdout/log_stream` in musubi-fleet → no capture of game-server stdout/stderr; relies on provider (docker logs / k8s)* — [source](https://hathora.dev/api (swagger.json, archived))
- ❌ **Hiro/Satori** · Databricks churn-prediction loop: export Parquet, run ML, write `dbChurnPrediction` custom property back via Console API — Guide — *none* — [source](https://heroiclabs.com/docs/satori/guides/satori-databricks/)
- ❌ **PlayFab** · Manage Events: per-event sampling ratio 0–100% (takes 4h), Event Partitioning (private preview, ≤20 events → separate tables/blobs) — Private/public preview — *No per-event sampling config (grep sampling: tracing only)* — [source](https://learn.microsoft.com/gaming/playfab/features/data/export-data/event-sampling-overview)
- ❌ **PlayFab** · Sessions: client_session_start / client_focus_change events auto-emitted by SDKs ≥180809 for screentime — SDK feature — *No client SDK → no automatic session/focus telemetry* — [source](https://learn.microsoft.com/gaming/playfab/features/data/playstream/sessions)
- ❌ **Pragma** · Dedicated telemetry backend — Separate shard/node type for telemetry traffic (2026.3), `DedicatedTelemetryMode` in SDK, `TelemetryBackendClientConfig.maximumConcurrentRe… — *no evidence recorded* — [source](https://pragma.gg/docs/2026.3.0/concepts/telemetry/telemetry-dedicated-backend)
- ❌ **Pragma** · Sampling — `globalWeight`, `perEventWeight`, `alwaysEnabledIds` (0–1000), hot-reload broadcast to connected sessions. — *no evidence recorded* — [source](https://pragma.gg/docs/2026.3.0/concepts/telemetry/telemetry-overview)
- ❌ **Pragma** · Session-service broadcast batching — `notifyAllActiveSessionsBatchSize`, `notifyAllActiveSessionsBatchIntervalMillis` on Player/Partner/Telemetry session configs. — *no evidence recorded* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/broadcast-notifications)
- ❌ **Pragma** · Storage plugins — `DbTelemetryPlugin` (MySQL), `S3TelemetryPlugin` (`eventBatchSize` 10000, `timeWindowPerBatchMillis` 60000), custom `TelemetryPlugin`. — *no pluggable event sink besides webhooks/Zanshin.* — [source](https://pragma.gg/docs/2026.3.0/concepts/telemetry/telemetry-plugin)
- ❌ **Pragma** · `TelemetryServiceConfig` — `globalWeight`, `perEventWeight`, `alwaysEnabledIds`, `disablePragmaAnalytics`; S3 `eventBatchSize`, `timeWindowPerBatchMillis`. — *no evidence recorded* — [source](https://pragma.gg/docs/2026.3.0/concepts/telemetry/telemetry-plugin)
- ❌ **Rivet** · Actor Inspector (local dev button, per-actor inspector token, custom inspector tabs shipped with the actor) — `examples/inspector-tabs` — *no inspector (`rg -i inspector` = 0)* — [source](https://rivet.dev/actors/docs/inspector-tabs/)
- ❌ **Rivet** · `rivet logs` CLI (follow, filter by actor/name/text, JSON, pool) — CLI — *no log CLI (`musubi-cli` has none)* — [source](https://rivet.dev/docs/debugging/)
- ❌ **SpacetimeDB** · SQL console in dashboard — Web SQL editor per DB — *no admin console (parity.md) — use psql*
- ❌ **UGS LiveOps** · Attribution / MMP acquisition-source events — `acquisitionSource` event — *No attribution fields; out of scope of core* — [source](https://docs.unity.com/en-us/analytics/user-attribution-support/acquisition-source-event)

**P1 · runtime/extensibility** — owner: musubi-runtime + musubi-idl (WASM host ops, `/v1/rpc/*`, hooks, `/v1/admin/webhooks`)

- ❌ **AccelByte** · Login allowlist: only accounts holding allowed roles can log in (closed tests/events) — Private Cloud only — *no role-based login allowlist (rg `allowlist` none)* — [source](https://docs.accelbyte.io/gaming-services/modules/foundations/tool-utilities/login-allowlist/)
- ❌ **GameLift** · Alias object & console — `Alias{AliasId, Name, Arn, Description, RoutingStrategy{Type SIMPLE/TERMINAL, FleetId, Message}, CreationTime, LastUpdatedTime}`; console c… — *`rg -i alias crates/musubi-fleet` → none* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-console-aliases.html)
- ❌ **PlayFab** · Sweepstakes API (preview, Xbox): CreateSweepstake/EnterSweepstake/GetSweepstakeEntries/Winners — raffles/giveaways — Private preview — *No sweepstakes/raffle primitive (reward tables could emulate)* — [source](https://learn.microsoft.com/rest/api/playfab/sweepstakes/)
- ❌ **Pragma** · Login data aggregation plugin — `LoginDataPlugin` dependent jobs collate inventory/custom data into `GetLoginDataV1`. — *no single aggregated login payload hook.* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/concurrency/dependent-jobs)

**P1 · SDKs** — owner: `sdk/` (generate from `GET /openapi.json` + IDL codegen; shared `musubi-client-core`)

- ❌ **Agones/OM** · Beta lists: `GetList`, `UpdateList`, `AddListValue`, `RemoveListValue` (+ language helpers `AppendListValue`, `DeleteListValue`, `SetListCapacity`, `ListContains`, `GetListLength`, `GetListValues`) — Capacity ≤ 1000 — *no evidence recorded*
- ❌ **Agones/OM** · `Reserve(seconds)` — `Reserved` state: not deletable on scale-down/update, not allocatable, auto back to Ready after N s (0 = forever); cancelled by Ready/Allocate — For external matchmakers that need a hold — *No reserved state (`rg -i reserve crates/musubi-fleet crates/musubi-server/src/http/fleets.rs` → 0 relevant hits)*
- ❌ **Beamable** · Offline mode matrix — Read-only cached support for A/B, Identity, Admin, Content, Currency, Inventory, Leaderboards, Stats, Store, Tournaments; none for Chat/Clo… — *No client cache layer* — [source](https://docs.beamable.com/docs/offline-mode)
- ❌ **brainCloud** · Wrapper persistence — stores anonymousId/profileId on device (`GetStoredProfileId`, `ResetStoredAnonymousId`), `AlwaysAllowProfileSwitch`, `Logout[OnApplicationQ… — *no SDK* — [source](https://docs.braincloudservers.com/api/wrapper)
- ❌ **EOS** · EOS SDK in C (stable ABI) + C# wrapper; async callback model w/ Tick; custom memory delegates; API version macros — .NET Framework 3.5+/.NET Core 3.1 for C# — *musubi has no shipped client SDK (README: "client SDKs ... not written yet"); only OpenAPI spec + Unreal scaffold* — [source](https://dev.epicgames.com/docs/epic-online-services/working-with-the-eos-sdk/conventions-and-limitations)
- ❌ **GameLift** · Fleet IAM role for game servers — `InstanceRoleArn` + `InstanceRoleCredentialsProvider SHARED_CREDENTIAL_FILE`; server SDK `GetFleetRoleCredentials` — *no credential vending to game servers (server key only)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-sdk-server-resources.html)
- ❌ **GameLift** · GetComputeCertificate / GetFleetRoleCredentials / ListContainersNetworkInfo — TLS cert path, temporary AWS creds, container port mappings — *no evidence recorded* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk5-cpp-actions.html)
- ❌ **GameLift** · InitMetrics / InitCustomLogger / Destroy — OTel metrics init, custom logger (5.6), SDK teardown — *no evidence recorded* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk5-cpp-actions.html)
- ❌ **GameLift** · Server SDK C# reference — C# actions & data types (5.5.1) — *no evidence recorded* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk5-csharp-actions.html)
- ❌ **GameLift** · Server SDK Go reference — Go actions & data types (5.6.0) — *no evidence recorded* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk-go-actions.html)
- ❌ **Hathora** · GetLogsForProcess / DownloadLogForProcess — `follow`, `tailLines` 1..5000; download text file — *no evidence recorded* — [source](https://hathora.dev/api (swagger.json, archived))
- ❌ **Hathora** · GetOrgs / GetOrgMembers / GetOrgPendingInvites / GetUserPendingInvites / InviteUser / RescindInvite / AcceptInvite / RejectInvite — E-mail invites with role; one org per user — *no team/invite model (`rg invite` → social only)* — [source](https://hathora.dev/api (swagger.json, archived))
- ❌ **Metaplay** · Player incident reports: client auto-captures crashes/connection failures/unhandled exceptions with logs, uploaded on next session, deduped by fingerprint, dashboard Incident Reports page with filters, custom incident types — R8; R33 UX — *no client incident ingestion endpoint (`rg -i incident` → none)* — [source](https://docs.metaplay.io/feature-cookbooks/unity-sdk-features/implementing-custom-incident-reports)
- ❌ **Rivet** · React `@rivetkit/react`: `useActor`, `useActorEvent`, Suspense, SSR — hooks — *no evidence recorded* — [source](https://rivet.dev/actors/docs/clients/react/)
- ❌ **SpacetimeDB** · Row/table callbacks — `on_insert/on_update/on_delete/on_before_delete`, `iter/count`, unique-index `find` — *no evidence recorded*
- ❌ **SpacetimeDB** · Rust client SDK — `spacetimedb-sdk` crate; async event loop; wasm32 target (REL 2.1.0); capability traits/generic accessors (2.7) — *no Rust client SDK (sim-sdk is server-side)*

**P1 · deployment/ops** — owner: musubi-cli + `deploy/` (Helm, Terraform), musubi-cluster

- ❌ **Edgegap** · Agent skills repo — `edgegap-agent-skills`: Unreal, FishNet, PurrNet, containerization, troubleshooting, local testing, deploy API, server design — *no evidence recorded* — [source](https://github.com/edgegap/edgegap-agent-skills)
- ❌ **Hathora** · `hathora log` — Tail/download process logs — *no log capture* — [source](https://hathora.dev/docs/how-hathora-works/telemetry)
- ❌ **Meshing vendors (coherence)** · Build sharing — upload game builds (WebGL) and share via Cloud — *no build hosting (`rg -i "build shar"` none)* — [source](https://docs.coherence.io/support/webgl)
- ❌ **Meshing vendors (Improbable SpatialOS)** · Console Inspector — live entities & component values, connected workers + load, LB strategies, areas of authority & interest; new Inspector with query editor (… — *data via admin routes only; no UI* — [source](https://documentation.improbable.io/spatialos-tools/docs/console-introduction)
- ❌ **Meshing vendors (Improbable SpatialOS)** · Logs viewer — web logs with timeline, filters (message, level, node_name, logger_name) — *stdout/OTLP only* — [source](https://documentation.improbable.io/spatialos-tools/docs/logs-viewer)
- ❌ **Metaplay** · Client Patch Versions per logic version, per-platform patch compatibility requirements editable on server, hotfix workflow — R31 — *single min version only* — [source](https://docs.metaplay.io/feature-cookbooks/game-updates-and-versioning/client-patch-versions)
- ❌ **Metaplay** · Developer Players: dashboard "Mark as Developer" (`api.players.set_developer`), `[DevelopmentOnlyAction]` allowed only in dev envs or for developers, log in during maintenance mode, DeveloperOnly offers/groups, stored in GlobalStateManager — *no developer-player flag (`rg -i 'developer' README` → dev tooling only); roles are operator-side* — [source](https://docs.metaplay.io/feature-cookbooks/developer-players/working-with-developer-players)
- ❌ **Metaplay** · Metaplay Docs Custom GPT, Docs MCP server (`metaplay llm-docs` glob/read/ripgrep/search), Portal MCP (inspect prod issues, roles, env status), Metaplay Agent skills for Claude Code/Codex/OpenCode (`metaplay skills install`) — R38 — *no MCP/agent tooling; only `openapi.json`* — [source](https://docs.metaplay.io/miscellaneous/ai-assistants/index)
- ❌ **Metaplay** · Server time skip (environment-wide, irreversible, dashboard Settings → Time Skip; dev/staging only) for testing schedules — R30 — *no clock override (`rg -i 'time_skip\|clock_offset'` → none)* — [source](https://docs.metaplay.io/feature-cookbooks/time-skip/testing-game-schedules-with-time-skipping)
- ❌ **Metaplay** · Simulating a poor network connection (client-side latency/packet loss simulation) — *`musubi bench` measures latency but has no client-side loss/latency injection (`rg -i 'packet.?loss\|jitter'` → none)* — [source](https://docs.metaplay.io/feature-cookbooks/unity-sdk-features/simulating-a-poor-network-connection)
- ❌ **Hiro/Satori** · GDD templates for designing systems; sample projects (Hiro Store, Mage Mayhem, Achievements, Teams, Challenges, Event Leaderboards, Inventory) — Getting started — *musubi has docs/ plan + examples in repo, no sample games* — [source](https://heroiclabs.com/docs/hiro/concepts/getting-started/templates/)
- ❌ **Hiro/Satori** · Hiro Exporter: Google Sheets -> Hiro JSON definitions (Economy, Achievements, Energy, Inventory) with direct upload to Heroic Cloud via StoragePersonalizer — Spreadsheet-driven design workflow — *musubi content bundles are JSON via API/CLI; no spreadsheet exporter* — [source](https://heroiclabs.com/docs/hiro/guides/hiro-exporter/)
- ❌ **Rivet** · AI skills package `npx skills add rivet-dev/skills`, GitHub Action to publish skills; docs "Copy Markdown" per page — agent-first docs — *no agent skill files; README is the only reference (`rg -il "skill"` only gameplay context)* — [source](https://rivet.dev/docs/skill/)
- ❌ **SpacetimeDB** · AI-agent integration — MCP server, Claude plugin, `.md` docs for agents — *no evidence recorded*
- ❌ **SpacetimeDB** · `spacetime mcp` (Model Context Protocol server) — stdio MCP server for coding agents; also on Maincloud (2.10); Claude plugin (2.8.2) — *grep `mcp` = 0*
- ❌ **Steamworks** · Engine integrations — Unreal OnlineSubsystemSteam, Unity via community, Source — *HTTP/WS only today* — [source](https://partner.steamgames.com/doc/sdk/api)
- ❌ **Steamworks** · SDK / platform coverage — native C++ SDK (Win32/Win64/macOS/Linux x64, linuxarm64 + androidarm64 since 1.63), flat C API, many community wrappers (Steamworks.NET, Fa… — *no client SDKs yet; Unreal scaffold only (docs/parity.md "SDK")* — [source](https://partner.steamgames.com/doc/sdk/api)

**P1 · console/admin** — owner: new `console/` SPA over `/v1/admin/*` (operator SSO + roles)

- ❌ **Beamable** · Announcements / Groups / Leaderboards / Tournaments / IAP (Real-Money Transactions) pages — Dedicated Portal pages under Engage/Monetize — *No admin console (README: console not yet written)* — [source](https://docs.beamable.com/docs/portal)
- ❌ **Beamable** · Beamable Console (successor) + MicroViews extensions — `@beamable/portal-toolkit`: `Portal.registerExtension()`, React 19 template + web components (`beam-btn`, `beam-data-table`), `ExtensionCon… — *No console, no extension framework* — [source](https://docs.beamable.com/docs/microviews)
- ❌ **Edgegap** · Web dashboard — Apps/versions, deployments (wizard, archive, filters), sessions, matchmakers, server browsers (card layout + API spec viewer), relays, clus… — *README: admin console not written; everything is API/CLI* — [source](https://docs.edgegap.com/getting-started)
- ❌ **Hathora** · Bullet Mania open-source sample — Scales to thousands of matches — *no evidence recorded* — [source](https://blog.hathora.dev/changelogs-april-2023/)
- ❌ **Hathora** · Create room from Console with per-region ping — Aug 2024: region list shows live ping — *API only* — [source](https://blog.hathora.dev/changelogs-august-2024/)
- ❌ **Hathora** · Deployment page redesign, app overview with buildTag, copy processId — UX iterations 2023–2024 — *no console* — [source](https://blog.hathora.dev/changelogs-may-2024/)
- ❌ **Hathora** · Fleet tab: capacity graphs, cloud minimums, node table — Oct 2024 / Jul 2025 — *API only* — [source](https://blog.hathora.dev/fleet-management-is-here-2/)
- ❌ **Hathora** · Minecraft "no code changes" demo deploy — Apr 2023 get-started — *no evidence recorded* — [source](https://blog.hathora.dev/changelogs-april-2023/)
- ❌ **Hathora** · Private networking mesh between nodes — WireGuard-style mesh across providers (Elastic Metal) — *`rg wireguard/vpn` → 0; Terraform private networks per provider only* — [source](https://blog.hathora.dev/elastic-metal-from-game-servers-to-global-infrastructure/)
- ❌ **Hathora** · Process table with filters, counts, copy id, logs, metrics, stop — Sep 2023–Mar 2025 iterations — *API only* — [source](https://blog.hathora.dev/changelogs-january-2024/)
- ❌ **Hathora** · Region filter → 10 most recent started/stopped processes — Sep 2023 — *API filtering by fleet only* — [source](https://blog.hathora.dev/changelogs-september-2023/)
- ❌ **Hathora** · Search stopped room/process → logs + metrics in one click — Sep 2023 — *no evidence recorded* — [source](https://blog.hathora.dev/changelogs-september-2023/)
- ❌ **Hathora** · Teams UI: invites, roles — Aug 2024 / Feb 2025 — *no console; roles via admin keys* — [source](https://blog.hathora.dev/changelogs-february-2025/)
- ❌ **LootLocker** · Public/Private profile toggle (private disables Steam inventory integration / hides items) — *No Steam Inventory Service integration (rg 'steam inventory' = 0)* — [source](https://docs.lootlocker.com/players/managing-players/how-to/manage-players)
- ❌ **LootLocker** · Referral Programs: create/update/delete programs, player referral invite (`get-referral-invite`), list player referrals, program stats, void referral — Admin+Game API only (no docs page) — *rg -i referral = 0* — [source](https://ref.lootlocker.com/admin)
- ❌ **Metaplay** · Assume Roles (test permissions when Auth off; per-tab header) — Dev-only — *no role impersonation* — [source](https://docs.metaplay.io/operating-a-live-game/security-and-compliance/managing-dashboard-permissions)
- ❌ **Metaplay** · Custom dashboard development: Vue 3 + TS project scaffolded by `metaplay init dashboard`, `gameSpecific.ts` registration of custom views/tabs/pages/sidebar, `@metaplay/meta-ui` component library (Storybook), Tailwind, `metaplay build dashboard` — *no dashboard* — [source](https://docs.metaplay.io/liveops-dashboard/developing-the-liveops-dashboard)
- ❌ **Metaplay** · Custom dashboard pages/components via Integration API (`addUiComponent`, placements, `displayPermission`), Metaplay UI component library (MetaUI), Tailwind, Storybook — *no console* — [source](https://docs.metaplay.io/liveops-dashboard/how-to-guides/customizing-the-liveops-dashboard-frontend)
- ❌ **Metaplay** · Database scan jobs UI: list/create/pause/cancel jobs (e.g. deletion sweeps, custom migrations), progress %, timeline — *no evidence recorded* — [source](https://docs.metaplay.io/game-server-programming/how-to-guides/database-scan-jobs)
- ❌ **Metaplay** · Events/offers page: view config + live behaviour, per-player preview of how an event appears for that player's state/timezone — *no native in-game event/offer system (LiveOps delegated to Zanshin)* — [source](https://docs.metaplay.io/liveops-dashboard/introduction-to-the-liveops-dashboard)
- ❌ **Metaplay** · Generated forms & views from C# types (schema-driven), server-side form validation, custom field decorators — *storage JSON Schema exists but no UI generation* — [source](https://docs.metaplay.io/liveops-dashboard/how-to-guides/working-with-generated-dashboard-forms-and-views)
- ❌ **Metaplay** · Generated forms/views from C# types (`[MetaFormDisplayProps]`, `[MetaValidate*]`, localized fields, logic-version awareness), dashboard actions with `[RequirePermission]` + audit event generation, custom player details tabs — *no evidence recorded* — [source](https://docs.metaplay.io/liveops-dashboard/how-to-guides/working-with-generated-dashboard-forms-and-views)
- ❌ **Metaplay** · Player details page with 5 customizable tabs, UiPlacement slots — Tabs renamed via runtime options; custom Vue components injected per placement — *no UI; player data only via `GET /v1/admin/accounts/{id}`* — [source](https://docs.metaplay.io/liveops-dashboard/configuring-the-liveops-dashboard)
- ❌ **Metaplay** · Server error logs page (`api.system.view_error_logs` permission), recent server errors/exceptions with counts — R27 — *logs only via stdout/OTel* — [source](https://docs.metaplay.io/miscellaneous/sdk-updates/release-notes/release-27)
- ❌ **Nakama** · `RegisterConsoleHttp` custom console HTTP handlers (3.34) — OSS Go — *modules expose RPC/hook surfaces only (rg -i console_http → none)* — [source](https://heroiclabs.com/docs/nakama/server-framework/go-runtime/function-reference/)
- ❌ **Photon** · Web dashboard: AppIds — create/manage AppIds per product, AppId settings, team sharing with permissions — *no admin console UI (README "not written yet"); admin via HTTP API/CLI only* — [source](https://doc.photonengine.com/photon/current/photon-cloud)
- ❌ **Pragma** · Portal: Data Rights page — Create export/delete requests, see status, download report. — *no evidence recorded* — [source](https://pragma.gg/docs/2026.3.0/concepts/portal/operator-portal/social-operator-portal)
- ❌ **Pragma** · Portal: Game Servers page — View connected game servers, capacity, instances; generate partner tokens. — *`GET /v1/fleets/*` API only.* — [source](https://pragma.gg/docs/2026.3.0/concepts/portal/operator-portal/game-operator-portal)
- ❌ **Pragma** · Portal: Game Titles / Shards / Access Lists — Register titles, shards, limited-access windows. — *no evidence recorded* — [source](https://pragma.gg/docs/2026.3.0/concepts/portal/operator-portal/social-operator-portal)
- ❌ **Pragma** · Portal: Players page (Game Operator) — Search by player ID, view login history, player data entities, run operator-only Player Data operations, clear data. — *API only.* — [source](https://pragma.gg/docs/2026.3.0/concepts/portal/operator-portal/game-operator-portal)
- ❌ **Pragma** · System logs viewer — Operator RPC `SystemReporter.GetLogsV1` / portal page to fetch recent logs. — *logs go to stdout/OTLP; no admin log-tail route (`rg -i 'GetLogs\|/logs'` none).* — [source](https://pragma.gg/docs/2026.3.0/concepts/portal/operator-portal/social-operator-portal)

**P1 · compliance** — owner: musubi-server compliance.rs/retention.rs (`/v1/account/export`, age gate, `[retention]`)

- ❌ **Hathora** · Basic DDoS protection on every node — UDP/ICMP/SYN floods, reflection, amplification, malformed packets, reputation-based blocklist — *`rg ddos` → 0; UDP edge connect tokens drop unauthenticated packets but no volumetric mitigation or reputation lists* — [source](https://hathora.dev/docs/how-hathora-works/security)
- ❌ **Metaplay** · Backup re-deletion safeguard (players deleted then restored from backup are re-deleted) — Since R9 — *no backup-restore re-deletion ledger* — [source](https://docs.metaplay.io/miscellaneous/sdk-updates/release-notes/release-9)
- ❌ **Metaplay** · Player IP geolocation: MaxMind GeoLite2 Country/City auto-download + weekly updates, auto-disable on stale DB, `LastKnownLocation` in PlayerModel, shown in dashboard, segmentable (`LastKnownCountry`), privacy guidance — *no GeoIP (`rg -i 'geoip\|maxmind'` → none); only region hints for fleet routing* — [source](https://docs.metaplay.io/feature-cookbooks/player-ip-geolocation/implementing-player-ip-geolocation)

**P2 · auth** — owner: musubi-auth (`/v1/auth/*`, `/v1/account/*`)

- 🟡 **AccelByte** · Account Identifier Tags: admin-defined tags to categorise user accounts (CRUD, query) — *account metadata/labels via profile custom attributes; no admin tag taxonomy for accounts*
- 🟡 **Beamable** · Player-init federation (`IFederatedPlayerInit`) — Customize initial account state (starting inventory/stats) using `Account` + properties (e.g. console-exclusive items) — *`hook_account_created`/after-auth hooks in WASM can grant items (verify) — no dedicated interface* — [source](https://docs.beamable.com/docs/federated-player-init)
- 🟡 **brainCloud** · Advanced auth with extraJson — `AuthenticateAdvanced(authenticationType, ids, extraJson)` — attach custom JSON at registration — *metadata settable after login via `PATCH /v1/account {metadata}`; no registration-time payload parameter* — [source](https://docs.braincloudservers.com/api/capi/authentication/authenticateadvanced)
- 🟡 **brainCloud** · Identity listing & expiry — `GetIdentities`, `GetExpiredIdentities` (expired Facebook tokens), `GetIdentityStatus[AndData]`, `GetIdentityData`, `RefreshIdentity` — *`GET /v1/account` lists identities; no token-expiry tracking / refresh of provider tokens* — [source](https://docs.braincloudservers.com/api/capi/identity/getexpiredidentities)
- 🟡 **brainCloud** · Player state / profile fields — `ReadUserState`, `UpdateUserName/PictureUrl/ContactEmail/LanguageCode/CountryCode/TimezoneOffset`, `UpdateIsTester`, `ResetUser`, `DeleteUs… — *`PATCH /v1/account {username, display_name, avatar_url, lang, timezone, location}`; no `isTester` flag, no `ResetUser` (wipe progress keep identity)* — [source](https://docs.braincloudservers.com/api/capi/playerstate)
- 🟡 **brainCloud** · User attributes (key/value tags for segmentation) — `UpdateAttributes`, `GetAttributes`, `RemoveAttributes`; drive Segments/Promotions — *`metadata` JSON on account; audiences (`[audiences]`) target by fields, but no attribute API with portal-driven segment rules* — [source](https://docs.braincloudservers.com/api/capi/playerstate/updateattributes)
- 🟡 **Colyseus** · Bring-your-own DB (any ORM/Firebase) from room lifecycle (`onAuth`/`onLeave`) — *modules reach external systems only via `http.fetch` allowlist (no direct DB drivers in WASM sandbox)* — [source](https://docs.colyseus.io/database/bring-your-own)
- 🟡 **Colyseus** · Exception handling: `onUncaughtException(err, methodName)` wraps all callbacks (since 0.15.55); typed exceptions OnCreate/OnAuth/OnJoin/OnLeave/OnDrop/OnReconnect/OnMessage/OnDispose/Timestep/TimedEvent with context props; otherwise process shuts down — *WASM traps are isolated per call (fuel/deadline/memory) and never crash the node — stronger isolation — but modules get no hook to observe their own trap; erro…* — [source](https://docs.colyseus.io/room/exception-handling)
- 🟡 **Colyseus** · `beforeUpgrade(req, context)` transport hook before WS handshake with same read-only context as `onAuth` (`token`, `headers`, `ip`); `context.ip` unified across transports (0.18.5) — 0.18.5 — *`musubi-edge` per-IP limits and CDN country headers; no user-code pre-upgrade hook (WASM modules run after session is established)* — [source](https://docs.colyseus.io/server/transport/ws)
- 🟡 **Edgegap** · Relay session API — `POST /v1/relays/sessions {users:[{ip}], filters, webhook_url}` → `session_id`, `authorization_token`, `ready`, `linked`, relay `{ip, Serve… — *`match_create` relayed match + `match_join`; no IP-based session with webhook* — [source](https://docs.edgegap.com/learn/distributed-relay/relay-edgegap-api)
- 🟡 **EOS** · Auth status change notifications + expiry notifications (EOS_Connect_AddNotifyAuthExpiration, AddNotifyLoginStatusChanged) — Access token ~1h, 10 min pre-expiry warning — *JWT exp 15 min + refresh; no SDK callback* — [source](https://dev.epicgames.com/docs/epic-online-services/eos-fundamentals/connect-interface/connect-guide/refresh-player-authentication)
- 🟡 **EOS** · Developer Authentication Tool (local tool to hold dev credentials for repeated logins; EOS_LCT_Developer host:port) — Desktop tool shipped with SDK — *musubi: musubi dev + device auth / fixtures (admin fixtures) but no credential-caching dev tool* — [source](https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/eos-epic-account-services/developer-authentication-tool)
- 🟡 **EOS** · Login flow guidance for orgs with multiple products (shared org-level keychain → same PUID mapping per product) — *multi-project [[projects]] but no cross-project shared identity keychain* — [source](https://dev.epicgames.com/docs/epic-online-services/eos-fundamentals/connect-interface/connect-reference)
- 🟡 **EOS** · Login status change notifications (EOS_Auth_AddNotifyLoginStatusChanged) + EOS_Auth_GetLoginStatus — Server-initiated sign-out notification — *musubi: session revoke/kick closes WS (Error/ session close) but no SDK-level status-change callback* — [source](https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/eos-epic-account-services/auth-interface/auth-reference)
- 🟡 **Hathora** · GetConnectionInfo (unauthenticated polling) — Public endpoint returns `status` (starting/active) + `exposedPort{host,port,name,transportType}` + `additionalExposedPorts` — *connect info reaches the client through `matchmaker_matched.server` / lobby start and group connect tokens (R233); no unauthenticated public poll endpoint by r…* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **LootLocker** · Connected Accounts: link multiple providers to one player (direct in-client or external linking via hosted site), disconnect provider — *`POST /v1/account/link/*`, unlink, merge exist (direct linking); no hosted external-linking website* — [source](https://docs.lootlocker.com/players/unified-player-accounts/overview)
- 🟡 **LootLocker** · Console web UI for Platform Settings (enable providers, enter keys/secrets per platform) — *Providers configured in `musubi.toml` / admin config routes; no admin console UI (README status: console not written)* — [source](https://docs.lootlocker.com/players/authentication/overview)
- 🟡 **LootLocker** · Remember-me sessions (24h default, 30 days with remember_me, configurable length) + check-previous-session API — *Refresh token TTL configurable globally in `[session]`; no per-login remember-me flag* — [source](https://docs.lootlocker.com/players/white-label-login/how-to/returning-player)
- 🟡 **LootLocker** · Transactional email service config (game name, sender name, reply-to, logo) — *SMTP/email provider config for magic links exists; no per-game sender/reply-to/logo branding of templates* — [source](https://docs.lootlocker.com/players/white-label-login/how-to/white-label-login)
- 🟡 **Metaplay** · Social login conflict resolution: `SocialAuthenticateResult.ConflictingPlayer`, `SocialAuthenticateResolveConflict(useOther)`, auto-resolve heuristics, conflict cleared at session end, dashboard shows conflict events — *link returns conflict error + `POST /v1/account/merge`; no session-scoped pending-conflict state with "ConflictingPlayer" preview* — [source](https://docs.metaplay.io/feature-cookbooks/social-logins/managing-conflicting-social-logins)
- 🟡 **Hiro/Satori** · Default username generator (Player00000001) — prefix + 8 digits on first login — *musubi auto-generates usernames on device auth (rg "username" account) but no configurable pattern* — [source](https://heroiclabs.com/docs/hiro/concepts/base/)
- 🟡 **Nakama** · Authoritative matches: runtime match handler (Go/Lua/TS) with `MatchInit/JoinAttempt/Join/Leave/Loop/Terminate/Signal`, tick rate 1–60 (Go/TS; Lua up to 30), in-memory state, `matchCreate` from runtime/RPC only, label (≤2048 bytes) + `matchLabelUpdate`, `matchSignal`, broadcast to subset, kick, `matchGet`, graceful terminate seconds — OSS — *musubi authoritative matches run WASM modules (`MatchInput`, `MatchInputAck`, `MatchTickHash`, `MatchState/Delta`, rollback/record/replay, spectators — README…* — [source](https://heroiclabs.com/docs/nakama/concepts/multiplayer/authoritative/)
- 🟡 **Photon** · Auth provider "fail if unavailable" — per provider: reject or allow anonymous when your auth service is down — *provider toggles exist; no fallback-to-anonymous mode* — [source](https://doc.photonengine.com/realtime/current/connection-and-authentication/authentication/custom-authentication)
- 🟡 **Photon** · Client prediction + resimulation — input authority predicts, rollback & resimulate on snapshot; proxies not simulated by default; ClientsOnlyRollbackUpdatedObjects (2.1.3) — *server-side rollback params + input buffering + tick hashes (README rollback); client-side prediction library absent (no SDK)* — [source](https://doc.photonengine.com/fusion/current/concepts-and-patterns/network-simulation-loop)
- 🟡 **Photon** · PlayFab integration — PlayFab Photon auth token + webhook forwarding guide — *playfab mentioned in 6 files (import/migration tooling) but no PlayFab auth provider* — [source](https://doc.photonengine.com/realtime/current/reference/playfab)
- 🟡 **Photon** · Spawn/despawn, prefab table, object pooling, NetworkObject refs, nested objects, scene objects — NetworkRunner.Spawn with input authority, INetworkObjectProvider pooling, prefab table baked — *world entities spawn/despawn + handover (`musubi-mesh`); no prefab/pooling (engine-side)* — [source](https://doc.photonengine.com/fusion/current/manual/network-topologies)
- 🟡 **PlayFab** · Login response options (GetPlayerCombinedInfoRequestParams): return player profile, inventory, VC, user data, read-only data, statistics, title data, character list in one login call — InfoRequestParameters — *README login returns account + session + flags; no bundled inventory/statistics payload (clients make separate calls)* — [source](https://learn.microsoft.com/gaming/playfab/features/authentication/login/login-basics-best-practices)
- 🟡 **PlayFab** · Multiple logins per device: session tickets per title player; GetPlayedTitleList across titles of a publisher — Publisher scope — *Projects are isolated; no publisher roll-up* — [source](https://learn.microsoft.com/gaming/playfab/features/data/playerdata/playfab-gdpr-deleting-and-exporting-player-data)
- 🟡 **PlayFab** · Player location (country/city/lat-long from IP at login), origination (first identity provider), campaign attribution (AdCampaignAttribution via AttributeInstall) — Auto-populated — *Account has `location` field (admin-editable) + country from CDN header for routing; not auto-geolocated at login, no install/campaign attribution* — [source](https://learn.microsoft.com/gaming/playfab/features/data/playerdata/player-profile)
- 🟡 **PlayFab** · Publisher ID: titles within a studio share a publisher, enabling cross-title master player account + publisher data — Master player account spans titles under one publisher — *musubi: multi-project config shares one DB/account scope; no explicit publisher-level data namespace (grep publisher: none)* — [source](https://learn.microsoft.com/gaming/playfab/features/data/playerdata/using-player-publisher-data)
- 🟡 **Pragma** · Login flow: single `LogInV1` returns social+game tokens and login data — Game client logs in once per provider; `GetLoginDataV1` collates data via Dependent Jobs (login data plugin). — *`/v1/auth/*` returns tokens + account; no pluggable "login data" aggregation step (clients fetch wallet/inventory/etc. separately or via module RPC).* — [source](https://pragma.gg/docs/2026.3.0/concepts/authentication/login)
- 🟡 **Pragma** · Published scale results — 1M CCU 5-hour test (0.0.94): 42 clients × 24k players, 378 logins/s, 12-min matches. — *README "Measured (not promised)" publishes benchmarks at smaller scale (single node / small cluster).* — [source](https://pragma.gg/docs/2026.3.0/tech-blog/load-testing/achieving-1-million-ccu)
- 🟡 **Rivet** · Cloud tokens: `sk_` secret (ns-scoped), `pk_` publishable (browser: resolve/create/connect), `ck_` connection (resolve+connect), `cloud_api_` (deploy/CI); `rivet token create --kind` — prefixes — *scoped keys and player JWTs exist; no browser-safe publishable key tier (clients always authenticate as players)* — [source](https://rivet.dev/docs/authentication/)
- 🟡 **SpacetimeDB** · Short-lived WebSocket token exchange — `POST /v1/identity/websocket-token` for header-less transports (Unity WebGL, URLs) — *access/refresh token rotation exists; no dedicated short-lived URL-safe token endpoint*
- 🟡 **Steamworks** · Message-oriented API — `ISteamNetworkingMessages` (UDP-like by identity, channels, auto-connect) — *`match_data_send` targeted/broadcast in relayed matches (≤64 players) covers the same use case via the server relay* — [source](https://partner.steamgames.com/doc/api/ISteamNetworkingMessages)
- 🟡 **Steamworks** · Session tickets (client → gameserver/backend) — `ISteamUser::GetAuthSessionTicket` → `BeginAuthSession` on game server or client; `ValidateAuthTicketResponse_t` reports OK / VAC banned /… — *musubi verifies Steam session tickets server-side (`musubi-integrations` steam.rs calls `AuthenticateUserTicket`) to mint its own JWT; there is no peer-to-peer…* — [source](https://partner.steamgames.com/doc/features/auth)
- 🟡 **Steamworks** · Steamworks partner user management — Steamworks groups, per-app permission matrix (Edit App Metadata, Publish, Manage Pricing, Manage Signing, View Financial, Manage Users…), A… — *operator SSO (OIDC) + console roles support/liveops/ops/admin (README Operations); coarser than Steam's ~15 fine-grained permissions, no per-app scoping inside…* — [source](https://partner.steamgames.com/doc/gettingstarted/managing_users)
- 🟡 **UGS LiveOps** · Digital Services Act (DSA) notifications to players — `GET /users/{id}/notifications`; SDK ≥3.3; restricted-player notices — *Ban reason/message is returned on login (`ban message`), in-app notifications exist; no DSA-specific statement-of-reasons object/ack flow* — [source](https://docs.unity.com/en-us/authentication/dsa-notifications)
- 🟡 **UGS LiveOps** · Service-account scopes on server sign-in — Auth SDK 3.6.0: specify scopes when signing in with a service account — *Server-key roles `server/admin/support/liveops/ops` are coarse; no per-scope selection at token issue* — [source](https://docs.unity.com/en-us/services/release-notes)
- 🟡 **UGS LiveOps** · Targeting attributes at authentication time (Targeting Rules → variant tags) — Auth SDK 3.7.0 `Targeting.SetEnabled(true)`; Targeting Rules Admin API: tags, tag groups, rules evaluated at auth — *Named audiences (`PUT /v1/server/audiences/{name}`) evaluate rules server-side and are used by mail/store/matchmaker; no "assign tags at login and expose to cl…* — [source](https://docs.unity.com/en-us/oas-targeting-rules-admin/1.0.0)
- 🟡 **UGS Multiplayer** · Topologies — client-server (dedicated / listen-server host) and Distributed Authority — *Dedicated (fleets), relayed client-host, authoritative WASM actor; no DA.* — [source](https://docs.unity3d.com/Packages/com.unity.netcode.gameobjects@3.1/manual/terms-concepts/network-topologies.html)

**P2 · accounts** — owner: musubi-account (`/v1/account`, `/v1/admin/accounts/*`)

- 🟡 **Photon** · Custom player properties — per-actor Hashtable synced to all; CAS; BroadcastPropsChangeToAll — *match presence metadata & party member metadata exist; no generic per-actor property bag in relay matches* — [source](https://doc.photonengine.com/realtime/current/gameplay/custom-properties)
- 🟡 **Photon** · PlayerTTL — ms an inactive actor keeps its slot/ActorNr before leave logic; -1 infinite — *match rejoin window exists (session TTL, `ttl` 60 files) but not a per-room configurable PlayerTTL* — [source](https://doc.photonengine.com/quantum/current/manual/game-session/reconnecting)
- 🟡 **Photon** · Samples — Asteroids, BR200 (200 players), Casual MMO (server plugin), Simple FPS, Tanknarok, Karts, Golf, Impostor, Quiz, Pirate Adventure, Razor Mad… — *examples/ + sim-sdk demos; nothing engine-side* — [source](https://doc.photonengine.com/fusion/current/technical-samples/dedicated-server/fusion-dedicated-server)
- 🟡 **UGS LiveOps** · Custom user id / custom attributes from client — `SetCustomUserID`, `userAttributes`, `appAttributes`, `filterAttributes` (payload reduction) — *Session metadata/`client` fields on login feed audiences (platform, lang, location); no arbitrary attribute bag evaluated per request* — [source](https://docs.unity.com/en-us/remote-config/code-integration)
- 🟡 **UGS LiveOps** · Game Overrides targeting with JEXL conditions — Categories `unity.*` (platform, appVersion, country, language…), `app.*`, `user.*`, `filter.*`; expression ≤500 chars; no nested attributes… — *Audiences (`PUT /v1/server/audiences/{name}`: lang, location, platform, age bracket, created window, track level, activity, metadata) — fixed rule set, no free…* — [source](https://docs.unity.com/en-us/remote-config/game-overrides-and-settings)
- 🟡 **UGS LiveOps** · Player-level policies (per-player overrides, list all player policies) — `PATCH .../players/{id}/resource-policy` — *Per-account `limits` (`/v1/admin/accounts/{id}/limits`) and bans/shadow; no per-player route allow/deny* — [source](https://docs.unity.com/en-us/oas-access-resource-policy/1.0.0)

**P2 · storage** — owner: musubi-storage (`/v1/storage/*`, IDL collections)

- 🟡 **Beamable** · Cloud Save upload/download URLs & manifest commit — `cloudsaving/data/uploadurl`, `downloadurl`, `commitmanifest`, `move`, `replace`, `metadata`, `uploadurlfromportal` — *Blob presigned S3 routes exist for UGC/storage blobs; no manifest/commit semantics* — [source](https://docs.beamable.com/reference/cloud-saving-overview)
- 🟡 **Beamable** · Content validation (`MustReferenceContent`, field validators, Issues badge) — Validation before publish; strict serialization — *JSON-schema validation for storage schemas and IDL configs; no reference validators* — [source](https://docs.beamable.com/docs/content-team-workflows)
- 🟡 **Beamable** · Environments dev→staging→prod & promotion — 3 realms auto-created per game; promote content between realms in Portal; live refresh server→client on publish — *`node.environment` + multi-project + content bundle import/export; promotion is manual export/import, no UI* — [source](https://docs.beamable.com/docs/content-overview)
- 🟡 **Colyseus** · GeoIPPlugin (`@colyseus/geoip`) — country from IP at auth via MMDB (BYO file / MaxMind auto-download weekly / DB-IP Lite daily), `client.geoip`, `lookup(ip)` — separate package — *country only from CDN header (`cf-ipcountry`/`x-country` → `[regions] country_map`), no MMDB lookup; client IP stamped on sessions for suspicious-login detecti…* — [source](https://docs.colyseus.io/room/plugins/geoip)
- 🟡 **Colyseus** · Inheritance: `.extend()` / `@entity`, collections of polymorphic subtypes registered automatically — *IDL has no inheritance/union-of-subtypes; JSON state can carry a discriminator field* — [source](https://docs.colyseus.io/state/schema)
- 🟡 **Colyseus** · `StateView` per-client visibility (0.16, replaces @filter): `.view(tag)` fields, `view.add/remove/has(instance, tag)`, specialized numeric tags, `view.subscribe(collection)`; per-view encode step, shared view instances cached; "not optimized for large datasets" — *matches: `sync_exclude` private paths only (per-player private state) — no per-client arbitrary visibility sets; worlds: full interest management (radius, `bud…* — [source](https://docs.colyseus.io/state/view)
- 🟡 **Hathora** · Applications — CRUD `AppsV2`; name unique, lowercase/digits/dashes ≤32; `authConfiguration`; `appSecret`; `serviceConfig` (load balancer) — *one musubi deployment = one game; multi-environment by separate clusters/`export/import`; no in-cluster "application" object* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · Default env vars injected into container — `HATHORA_APP_ID/APP_SECRET/PROCESS_ID/DEPLOYMENT_ID/BUILD_TAG/ALLOCATED_CPU/ALLOCATED_MEMORY_MB/REGION/ROOMS_PER_PROCESS/INITIAL_ROOM_ID/IN… — *`MUSUBI_SERVER_ID/FLEET_ID/PORT/API/SERVER_KEY` + FleetSpec env (R233, R388); no region/build/cpu/hostname/initial-payload variables — server must call `GET /v…* — [source](https://hathora.dev/docs/guides/access-env-variables)
- 🟡 **Metaplay** · Config history: active/archived/unpublished status, who built/published when; alerts at >100 unpublished / >200 archived with bulk cleanup — *content versions table with publish/rollback + prune, no author/status workflow or thresholds* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/managing-game-configs-from-the-dashboard)
- 🟡 **Metaplay** · Configs loaded from disk at startup if newer than active (ship config with server build) — *`musubi deploy` manifest ships modules/fleets; content via import* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/working-with-game-configs)
- 🟡 **PlayFab** · GetTime (server UTC clock for clients) — Client/Server — *No dedicated server-clock endpoint (grep server_time in realtime.rs: none); clients rely on HTTP `Date` header / realtime message timestamps* — [source](https://learn.microsoft.com/rest/api/playfab/client/title-wide-data-management/get-time)
- 🟡 **Pragma** · Batch operations — `doBatchOperation`: several operations, one transaction, all-or-nothing. — *single-object writes are transactional with versions; a WASM RPC can do multi-collection writes in one tx, but there is no client-facing atomic multi-object ba…* — [source](https://pragma.gg/docs/2026.3.0/concepts/player-data/how-to-guides/batch-operation-calls)
- 🟡 **Pragma** · Dynamic (hot) config reload — Many values reload without restart (`ConfigHandler.onConfigChanged`); docs mark which need restart. — *modules & content bundles hot-reload; core server config is read at start (`rg -i reload` → modules/watch only).* — [source](https://pragma.gg/docs/2026.3.0/concepts/configuration/configuration-overview)
- 🟡 **Pragma** · Encrypted secrets in config — Homebase `/secrets/encryptV1` per shard; encrypted strings in YAML; VPN-gated onboarding. — *secrets via env/K8s secrets + module secret store (`/v1/admin/runtime/modules/{name}/secrets`); no at-rest encryption of config values.* — [source](https://pragma.gg/docs/2026.3.0/concepts/configuration/backend-configuration)
- 🟡 **Pragma** · HTTP compression — gzip on gateways and backend clients, off by default; `httpCompressionMinBodyBytes` 200. — *WS per-frame compression negotiated (README "Realtime transport: channels and compression"); HTTP gzip layer not found (`rg -i 'CompressionLayer\|gzip' app.rs`…* — [source](https://pragma.gg/docs/2026.3.0/concepts/configuration/http-compression)
- 🟡 **SpacetimeDB** · Event tables (transient broadcast rows) — `#[table(event)]`: rows inserted+deleted inside the tx, clients get only `on_insert`; requires WS v2; cannot feed views/joins; event flag i… — *musubi has typed server→client events (`Notification`, `MatchData`, `PartyData`, `ChannelMessage`, world deltas) but no user-defined transient event stream tie…*
- 🟡 **SpacetimeDB** · Public / private tables — Private = server-only; public = readable by any client (still only mutable via reducers) — *storage collections have read/write permission levels (`musubi-storage`, `StorageSubscribe`), but no table-level visibility flags*
- 🟡 **Steamworks** · Cross-platform sync control — `SetSyncPlatforms(file, mask)` per file; Auto-Cloud OS restriction — *storage is platform-agnostic by default; no per-object platform mask (`rg -i sync_platform` → 0)* — [source](https://partner.steamgames.com/doc/features/cloud)
- 🟡 **Steamworks** · Developer-only cloud mode — enable cloud only for Developer Comp owners before launch — *dev fixtures/`musubi dev`; feature flags via LiveOps (Zanshin) could gate* — [source](https://partner.steamgames.com/doc/features/cloud)
- 🟡 **Steamworks** · Shared cloud between app ids — "Shared cloud APP ID" (demo ↔ full game) — *storage is scoped per project (multi-project binary); demo + full game would share a project — no explicit cross-project share* — [source](https://partner.steamgames.com/doc/features/cloud)
- 🟡 **UGS LiveOps** · Access Control configuration deploy from files — `ugs deploy` of policy JSON — *Policies in config/bundles only for storage* — [source](https://docs.unity.com/en-us/services/access-control)
- 🟡 **UGS LiveOps** · Environments — ≤25 per project; `production` default; environment selector in Editor; Environments API — *`node.environment` label + separate `[[projects]]` per deployment (12 source files mention environment); environments are separate clusters/DBs, not switchable…* — [source](https://docs.unity.com/en-us/remote-config/environments)
- 🟡 **UGS LiveOps** · Live Content / Live Releases (config+asset files backed by schemas, release snapshots, release pointers, tags, release diffs) — Live Content Admin 1.3.4: configs/files CRUD, batch, schemas; Live Releases: releases, release pointers, tags, diff between releases — *Content bundles (`/v1/admin/content`, `/versions`, `/rollback`, import, diff) ≈ releases; no release *pointers* (named indirection like `stable`/`beta`) resolv…* — [source](https://docs.unity.com/en-us/oas-live-releases-admin/1.0.0)

**P2 · social** — owner: musubi-social (`/v1/friends*`, `/v1/notifications*`, realtime `status_*`/`party_*`)

- 🟡 **AccelByte** · Social service (99 ops): StatConfiguration (10) incl. import/export CSV, UserStatistic (44: bulk fetch/update/reset, by stat codes, additional data), GlobalStatistic (4), StatCycleConfiguration (14) incl. stop/resume cycle, UserStatisticCycle (3); GameProfile/Slot (deprecated) — *stats set/incr/max/min + metadata; cycles/global stats missing; bulk by many users: partial*
- 🟡 **Beamable** · Connectivity / heartbeat — `ConnectivityService` heartbeat, auto reconnect, overridable `IConnectivityChecker` — *WS heartbeat/reconnect protocol exists server-side; no client SDK* — [source](https://docs.beamable.com/docs/connectivity)
- 🟡 **Beamable** · Group recommended & availability (name/tag availability) — `group-users/{id}/recommended`, `/availability` — *Name uniqueness check exists; no recommendations* — [source](https://docs.beamable.com/reference/groups-overview)
- 🟡 **brainCloud** · Group leaderboards — `PostScoreToGroupLeaderboard`, `GetGroupLeaderboardView[ByVersion]`, `GetGroupSocialLeaderboard`, dynamic group leaderboards (DaysUTC/Using… — *leaderboards filter by group members (`http/leaderboards.rs`), group challenges/wars; no group-as-entrant leaderboard with group scores* — [source](https://docs.braincloudservers.com/api/capi/leaderboard/postscoretogroupleaderboard)
- 🟡 **brainCloud** · Group monitoring (portal) — Groups & Group Entities & Group Files screens, remove members, jump to entities — *admin group routes exist; no UI* — [source](https://help.getbraincloud.com/en/collections/3933091-portal-x-pages)
- 🟡 **EOS** · Social restrictions surfaced in overlay (social ban days remaining, parental-control / console-settings disabled social features) — Reads Sanctions + platform settings — *musubi has bans/age-bracket feature closure in API responses; nothing rendered* — [source](https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/social-overlay/social-restrictions)
- 🟡 **Metaplay** · OTA delivery: client auto-downloads published config at session start; connected sessions keep negotiated version; optional force-disconnect to push immediately — *content versions served via `/v1/content/*` + `/v1/configs`; no session-pinned negotiated version or force-disconnect* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/working-with-game-configs)
- 🟡 **Pragma** · Disable incoming friend invites — Per-player toggle (`SetIncomingFriendInvitesEnabledV1`), SDK event `OnIncomingFriendInvitesEnabledChanged`. — *blocklist and appear-offline exist, but no account-level "refuse all friend requests" flag (`rg -i friend_request` → notification kind only).* — [source](https://pragma.gg/docs/2026.3.0/concepts/social/friends/friends-tasks)
- 🟡 **Pragma** · Operator-assigned roles via Account Plugin — Plugin assigns roles on login (e.g. by email domain). — *roles belong to API keys, not accounts.* — [source](https://pragma.gg/docs/2026.3.0/concepts/accounts/roles-and-permissions)
- 🟡 **Rivet** · Docs site with per-page "Copy Markdown", LLM-friendly skill, AsyncAPI/OpenAPI published, Typedoc (retired) — docs — *single long README + `docs/parity.md` + `GET /openapi.json`; no docs site* — [source](https://rivet.dev/docs/skill/)
- 🟡 **Steamworks** · Steam Level / badges / owned games / playtime — `IPlayerService` GetSteamLevel, GetBadges, GetCommunityBadgeProgress, GetOwnedGames, GetRecentlyPlayedGames, GetSingleGamePlaytime — *progression tracks/levels and seasons per game (README Progression); no cross-game library/playtime data* — [source](https://partner.steamgames.com/doc/webapi/IPlayerService)

**P2 · groups** — owner: musubi-social groups (`/v1/groups/*`, `/v1/alliances/*`)

- 🟡 **GameLift** · IAM access control — Identity policies, resource ARNs, condition keys, `iam:PassRole` for fleet roles, AWS managed policies `GameLiftGameServerGroupPolicy`, ser… — *API keys with roles `server/admin/support/liveops/ops`, operator SSO via OIDC, per-project keys; no attribute/condition policy language* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/security-iam.html)
- 🟡 **LootLocker** · Asset model: Context (grouping, detachable, user-facing, depends-on feature unlock) × Complexity (Generic, Loot Box, Drop Table; Customization/Package/Soft Currency Package/Rental EOL 2026-04-15); fields: name, price, discount price, suggested, Universal (dated auto-grant to all players), Featured (dated), Purchasable, Unique Instance, Platform Properties, Filters, Key/Value storage, Files (≤15 MB), Data Entities (blobs) — *Items with kind/tags/attributes JSON, unique vs stackable, versions/rollback, convert/migrate (`/v1/server/items/*`); no per-item file attachments/blob entitie…* — [source](https://docs.lootlocker.com/content/working-with-assets/how-to/understanding-assets)
- 🟡 **Hiro/Satori** · Team activity tracking (last active) and membership limits per player — 1.26 — *group events journal (`GET /v1/groups/{id}/events`); no per-player team-count cap (rg "max_groups" -> none)* — [source](https://github.com/heroiclabs/hiro/blob/master/CHANGELOG.md)
- 🟡 **Hiro/Satori** · Team leaderboards with per-member contribution tracking — TEAMS_LEADERBOARD_*, around-team; regional + tournament support — *README group-owned leaderboard records / team tournaments (0075) and group challenges; per-member contribution breakdown not stored (rg "contribut" -> none)* — [source](https://heroiclabs.com/docs/hiro/concepts/teams/leaderboards/)
- 🟡 **Photon** · Built-in providers — Steam, Facebook, Facebook Gaming, Oculus, Viveport, Epic EOS, Nintendo, PlayFab (via custom auth); consoles via gated docs — *musubi: Steam, Google, Apple, Facebook, Game Center, Xbox XSTS, PSN, Nintendo, OIDC, passkeys, email, magic link, device, Discord (`musubi-auth`); missing Ocul…* — [source](https://doc.photonengine.com/realtime/current/connection-and-authentication/authentication/overview-auth)
- 🟡 **SpacetimeDB** · Multi-database per server — One standalone/Maincloud server hosts many databases, each with its own module; `--parent` DB inherits team permissions; `spacetime.json` c… — *musubi is one backend deployment per game (worlds/cells shard inside it); no multi-tenant "databases" concept, but content environments (`0085 content envs`) +…*
- 🟡 **Steamworks** · Custom signaling for P2P — `ConnectP2PCustomSignaling`/`ReceivedP2PCustomSignal` for non-Steam platforms — *WebRTC transport implies musubi signalling for client↔server DataChannels (README Transport); no peer↔peer signalling relay service* — [source](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)

**P2 · chat** — owner: musubi-chat (`/v1/channel/*`, realtime `channel_*`)

- 🟡 **PlayFab** · Default languages: title default language + player preferred language (SetProfileLanguage ISO 639-1), used by templates/news/catalog — zh unsupported — *musubi account `lang` field exists; no title default language or localized content fallback logic* — [source](https://learn.microsoft.com/gaming/playfab/features/engagement/news/setting-default-languages)
- 🟡 **UGS Multiplayer** · Access tokens (VAT) — claims login/join/join_muted/kick/mute/mute_all/drop_all/transcription; issuer + key from dashboard; key rotation / Key Update API; server… — *JWT infra + JWKS exist; voice tokens are delegated to Aurix.* — [source](https://docs.unity.com/en-us/vivox-core/access-token-guide/access-token-format/overview)
- 🟡 **UGS Multiplayer** · Player evidence report API — per-player evidence across channels — *Admin message read + reports.* — [source](https://docs.unity.com/en-us/vivox-core/text-chat-guide/chat-history/text-evidence-management)

**P2 · leaderboards/tournaments** — owner: musubi-leaderboard (`/v1/leaderboards/*`, `/v1/tournaments/*`, `/v1/server/stats/*`)

- 🟡 **AccelByte** · Bulk stat update endpoints (multi-user, multi-stat), update strategies OVERRIDE/INCREMENT/MAX/MIN, reset user stat, admin stat edit, stat import/export (CSV) — spec social.json — *set/incr/max/min via `/v1/stats`; no CSV import/export of stat configs* — [source](https://docs.accelbyte.io/gaming-services/modules/online/statistics/implementing-server-authoritative-player-statistics/)
- 🟡 **Beamable** · Events (one-off/recurring, phased) — Phases with duration, partition size (e.g. 50), rules, rewards (currency/items) by score & rank, group events — *Live events + tournaments + leaderboards cover it, but no multi-phase event entity with partitions* — [source](https://docs.beamable.com/docs/events)
- 🟡 **Steamworks** · Aggregated (global) stats — "Aggregated" flag → `RequestGlobalStats(days history)` / `GetGlobalStat`; Web API `GetGlobalStatsForGame` (name[] list, startdate/enddate) — *`GET /v1/server/stats/top` ranks players per stat; no automatic daily global totals history per stat* — [source](https://partner.steamgames.com/doc/features/achievements)
- 🟡 **Steamworks** · Friends-only reads — "Reads = Friends" → clients may only download friends' entries — *`?friends=1` filter exists, but no per-leaderboard setting that *forbids* global reads to clients* — [source](https://partner.steamgames.com/doc/features/leaderboards)
- 🟡 **UGS LiveOps** · Authoring: `.lb` assets in Editor, CLI deploy, dashboard — TieringConfig JSON etc. — *`musubi deploy` manifest; no editor asset type* — [source](https://docs.unity.com/en-us/leaderboards/leaderboards-assets)

**P2 · matchmaking** — owner: musubi-matchmaker (`[[matchmaker.queues]]`, `/v1/server/matchmaker/*`, `/v1/lobbies/*`)

- 🟡 **AccelByte** · Join-in-progress opt-out (`new_session_only` ticket attribute), session exclusion (exclude specific sessions, past N sessions recorded client-side, party-synced) — *backfill can be disabled per queue; no per-ticket `new_session_only` flag or excluded-session list (rg `new_session_only\|exclude` in matchmaker: none)* — [source](https://docs.accelbyte.io/gaming-services/modules/multiplayer/matchmaking/opt-in-opt-out-join-in-progress/)
- 🟡 **brainCloud** · Matchmaking Monitor & Server Monitor (portal) — 5.4–5.6: lobby-type funnel stats, time-to-match, launch stats; Server Monitor with node CPU/mem/storage (New Relic), container counts, laun… — *`GET /v1/server/matchmaker/stats` (p50/p95/p99 wait per queue×region), Prometheus metrics, matchmaker simulation; no UI* — [source](https://updates.braincloudservers.com/braincloud-5.6-is-live-1Dnhh6)
- 🟡 **Colyseus** · Room-type options `filterBy([...])` (stored client options filter later joins; `maxClients` from room instance) and `sortBy({clients: -1})` — *`GET /v1/matches?label&min_size&max_size&open_slots&region` label filtering + lobbies filter; no server-defined sort priority for auto-join* — [source](https://docs.colyseus.io/matchmaker)
- 🟡 **Colyseus** · Server-side matchMaker API: `createRoom`, `joinOrCreate`, `reserveSeatFor(room, options, authData)`, `join`, `joinById`, `create`, `query(conditions, sort)`, `findOneRoomAvailable`, `getRoomById`, `getLocalRoomById`, `findRoomsByIds` (1 wire op), `remoteRoomCall(roomId, method, args)` — *server key: `POST /v1/server/matches`, `GET /v1/server/matches/{id}`, `DELETE`, `/label`, `/results`, `POST /v1/server/matchmaker/*`, lobby start; no generic r…* — [source](https://docs.colyseus.io/matchmaker)
- 🟡 **Edgegap** · Auto-assigned reservation — Player IDs + policy name → immediate connection details; status code conveys scaling state + `Retry-After` — *`POST /v1/server/allocate {backfill, wait_secs}` is server-key; players go through matchmaker/lobby instead* — [source](https://docs.edgegap.com/learn/server-browser)
- 🟡 **Edgegap** · Bulk reservation confirmation by server — Returns accepted/expired/unknown IDs; server owns slot capacity — *`POST /v1/gameserver/players` updates counts; no reservation reconciliation* — [source](https://docs.edgegap.com/learn/server-browser)
- 🟡 **Edgegap** · Config: semver `version`, `inspect` mode, `max_deployment_retry_count`, `allowed_cors_origins`, `ticket_expiration_period`, `ticket_removal_period`, `group_inactivity_removal_period`, per-profile app/version binding — Per-profile expiration/removal periods — *`QueueConfig` has wait/widening/ready-check/teams/MMR/ping; no ticket expiration/removal periods, no CORS config (`rg cors` → none), no retry count* — [source](https://docs.edgegap.com/learn/matchmaking/matchmaker-in-depth)
- 🟡 **Edgegap** · Deployment selectors (tag-based session routing) — Session `selectors` match deployment tags to link sessions — *Allocation routed by `pool` + `region` only* — [source](https://api.edgegap.com/swagger.json)
- 🟡 **Edgegap** · Rule: `intersection` — Overlapping sets (maps, regions, modes); resolved value injected as `MM_INTERSECTION` — *Wildcard/`-field` queries; no set-intersection operator producing a resolved value* — [source](https://docs.edgegap.com/learn/matchmaking)
- 🟡 **Edgegap** · Search & browse with filter/sort language — OData-like `eq ne lt le gt ge`, `and`, `$order=metadata.x asc`, `rank(metadata.city,'Chicago','Toronto')`; cursor pagination — *Fixed query params on `GET /v1/matches`/`/v1/lobbies`; no expression language or custom sort* — [source](https://docs.edgegap.com/learn/server-browser)
- 🟡 **EOS** · Best display name resolution (online platform name vs Epic display name; EOS_UserInfo_CopyBestDisplayName / Ext) — Consistent cross-platform naming — *musubi profile has display_name + linked provider ids; no per-platform display-name selection API* — [source](https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/crossplay/best-display-name)
- 🟡 **EOS** · Permission levels: PublicAdvertised (searchable), JoinViaPresence (unlisted, friends join via overlay/presence, join by id), InviteOnly — Invite always allowed — *musubi public/private(+6-letter code, password); no presence-join / unlisted-but-joinable-by-id tier* — [source](https://dev.epicgames.com/docs/epic-online-services/multiplayer/lobbies-and-sessions/lobby-interface/lobby-interface-reference)
- 🟡 **GameLift** · Attribute value — `AttributeValue{S ≤100, N double, SL ≤100 strings, SDM map}` — *string/number props only* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_AttributeValue.html)
- 🟡 **GameLift** · Destination order & FleetIQ priority — `PriorityOrder` of LATENCY, COST, DESTINATION, LOCATION (1–4) + `LocationOrder` ≤100; Spot viability ("FleetIQ") first — *region choice by ticket ping props; no cost/destination priority* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/queues-design-priority.html)
- 🟡 **GameLift** · FlexMatch CloudWatch metrics — CurrentTickets, MatchAcceptancesTimedOut, MatchesAccepted/Created/Placed/Rejected, PlayersStarted, TicketsFailed/Started/TimedOut, TimeToMa… — *`GET /v1/server/matchmaker/stats` (wait p50/p95/p99, rejections) + Prometheus; no per-rule evaluation counters* — [source](https://docs.aws.amazon.com/gamelift/latest/flexmatchguide/match-events.html)
- 🟡 **GameLift** · Party aggregation in rules — Rules can average party attributes (`avg(teams[*].players.attributes[skill])`) — *party tickets kept together; MMR averaged? (`Teams` balancing by MMR) – no custom aggregation* — [source](https://docs.aws.amazon.com/gamelift/latest/flexmatchguide/match-examples-8.html)
- 🟡 **GameLift** · Player attributes — `string / number / string_list / string_number_map`, optional default; passed to game session via matchmaker data — *ticket `props` (string/number) used by query and `MmrRule.property`; no list/map typed attributes* — [source](https://docs.aws.amazon.com/gamelift/latest/flexmatchguide/match-rulesets-components-attributes.html)
- 🟡 **GameLift** · Player object in requests — `Player{PlayerId, PlayerAttributes map<AttributeValue>, Team, LatencyInMs map}` — *ticket `props` flat map; team assigned on match* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_Player.html)
- 🟡 **GameLift** · Queue notifications — SNS topic per queue (`NotificationTarget`) + automatic EventBridge — *global webhooks/event bus, not per-queue targets* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/queue-notification.html)
- 🟡 **Nakama** · i3D.net / PurrNet integration guides — partner — *no i3D-specific provider (webhook/SSH generic provider instead)* — [source](https://heroiclabs.com/docs/nakama/guides/concepts/i3d-integration/)
- 🟡 **Pragma** · Party plugin hooks — Kotlin `PartyPlugin`: onAddPlayer, onRemovePlayer, updateParty, updatePlayer, canChangeGameServerVersion, preferredGameServerZones, buildEx… — *WASM hooks on party events exist (`party.*` hooks) but fewer interception points (no per-field validation callback).* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/multiplayer-reference/multiplayer-backend-reference)
- 🟡 **Pragma** · `LoginQueueServiceConfig` — `targetLoginRatePerSecond`, `targetCcuLimit`, `estimatedSecondsToLogin`, `maxCheckTicketPollTimeSeconds`, `enableVipQueueBypass`, `vipUsers… — *capacity + poll interval only.* — [source](https://pragma.gg/docs/2026.3.0/concepts/authentication/how-to-guides/get-started-with-login-queues)
- 🟡 **Steamworks** · Party beacons (ISteamParties) — `CreateBeacon(openSlots, location=Steam chat group, connectString, metadata)`, `JoinParty` → connect string, `ChangeNumOpenSlots`, shown in… — *parties exist (`party_create/join/promote/data_send…`, join requests) but no public "beacon" discovery surface in a social group — nearest is group announcemen…* — [source](https://partner.steamgames.com/doc/api/isteamparties)
- 🟡 **Steamworks** · Per-member metadata — `SetLobbyMemberData/GetLobbyMemberData` — *member `team` and `ready` fields only (`…/team`, `…/ready`); no arbitrary per-member KV (`rg -i member_data crates/musubi-server/src/lobby.rs` → 0)* — [source](https://partner.steamgames.com/doc/api/ISteamMatchmaking)
- 🟡 **Steamworks** · Rich-presence join — `SetRichPresence("connect", …)` → "Join game" in friends list, `GameRichPresenceJoinRequested_t` — *rich status JSON ≤2 KB can carry a connect payload, but there is no client to render "Join" (SDKs not written)* — [source](https://partner.steamgames.com/doc/api/ISteamFriends)
- 🟡 **UGS Multiplayer** · Access control policies — service-account-only create/join via UGS CLI `ugs access` project policies; restrict player writes — *Operator roles (server/admin/support/liveops/ops), server keys and per-key quotas; no declarative per-endpoint deny policy for player tokens.* — [source](https://docs.unity.com/en-us/mps-sdk/access-control)
- 🟡 **UGS Multiplayer** · Config-as-code deployment — `.mmq` queue / `.mme` environment files, Deployment window, `ugs deploy`, "Fetch from Remote", inspector for hosting types (MPS 2.1.2) — *Content bundles (`GET/POST /v1/admin/content`, dry_run, rollback) cover economy/rating queues; matchmaker queues are API-managed, not in the bundle format.* — [source](https://docs.unity.com/en-us/mps-sdk/deployment/deploy-matchmaker-queue)
- 🟡 **UGS Multiplayer** · Custom session properties with visibility + indexing — ≤20 props; non-indexed ≤2 KB each, indexed ≤128 B; visibility `public`/`member`/`private`; `index` S#/N# — *Lobby metadata is one JSON blob ≤16 KB (bigger) but no per-property visibility and no indexed fields.* — [source](https://docs.unity.com/en-us/mps-sdk/session-data-and-player-data)
- 🟡 **UGS Multiplayer** · External data in rules (server-authoritative skill) — `Players.CustomData.ExternalData.Leaderboard.Score/Tier` (missing players auto-added with 0), `ExternalData.CloudSave.{path}` with accessCl… — *Server-side rating (Elo/Glicko-2/OpenSkill, seasons) is pulled into tickets automatically — more than Unity's leaderboard score — but no generic "read any stor…* — [source](https://docs.unity.com/en-us/mps-sdk/matchmaking/mm-rules)
- 🟡 **UGS Multiplayer** · Lobby `version` counter — increments on any non-private change; usable for optimistic sync — *Storage objects have CAS versions; lobby object has `updated_at` but no monotonic version field documented.* — [source](https://docs.unity.com/en-us/oas-lobby/1.0.0)
- 🟡 **UGS Multiplayer** · Lobby → voice/notification tokens — `POST /{lobbyId}/tokens` tokenType `vivoxJoin` / `wireJoin` — *`POST /v1/voice/token {party_id / match_id}` (Aurix) — not for lobbies; lobby chat room exists natively.* — [source](https://docs.unity.com/en-us/oas-lobby/1.0.0)
- 🟡 **UGS Multiplayer** · Rule samples library — sample JSON for skill, region, roles, party sizes — *README examples for queue config; no sample library.* — [source](https://docs.unity.com/en-us/mps-sdk/matchmaking/matchmaking-rule-samples)
- 🟡 **UGS Multiplayer** · Session Observability API (deprecated) — session detail timeline: MatchCreated(appliedRules), PlayersAssigned, Allocation*, PlayersConnected/Disconnected; role "Multiplayer Session… — *Match journals / recordings + audit log, not a matchmaking timeline per session.* — [source](https://docs.unity.com/en-us/oas-session-observability-public/1.0.0)

**P2 · realtime/matches** — owner: musubi-actor + musubi-transport (realtime `match_*` envelopes, `/v1/matches/*`)

- 🟡 **brainCloud** · One-way offline matches (CoC-style) + Playback Streams — `StartMatch`, `CancelMatch`, `CompleteMatch`; `StartStream/AddEvent/EndStream/ReadStream/ProtectStreamUntil/DeleteStream`, `GetRecentStream… — *replay recording of authoritative matches (`musubi-actor` replay, README) covers playback; no attacker/defender one-way match orchestration* — [source](https://docs.braincloudservers.com/api/capi/onewaymatch)
- 🟡 **Colyseus** · Patch cost model documented (1 B field header, refId sizes, varint number sizes, `sort()` resends all), change-tracking control (`untracked()`, `pauseTracking/resumeTracking`, `setDirty`), `.noSync()`, join payload reduction — *postcard binary + lz4 and RFC 6902 ops; no author-side tracking control (diff is computed from state snapshots each tick)* — [source](https://docs.colyseus.io/state/optimization)
- 🟡 **Colyseus** · Primitive types: string (≤4 GiB), number (varint auto), boolean, int8/16/32/64, uint8/16/32/64, float32/64, bigint64/biguint64 — *IDL scalar types (string/int/float/bool/bytes…) — no explicit 8/16-bit wire widths, postcard varint encoding instead* — [source](https://docs.colyseus.io/state/schema)
- 🟡 **Colyseus** · Room Plugins (`definePlugins`, `RoomPlugin` base): contribute messages, lifecycle hooks (before/after order per hook), public methods, `static dependencies` auto-include, `attachToTestRoom` test helper — ships in `colyseus` — *before/after hooks exist at server level (`hook_*` module exports, `before_`/`after_` for any API) but no in-match plugin composition model* — [source](https://docs.colyseus.io/room/plugins)
- 🟡 **Edgegap** · Relay session status in dashboard — Status per session — *`GET /v1/server/matches` lists matches; no UI* — [source](https://docs.edgegap.com/docs/release-notes/archive)
- 🟡 **Hathora** · Custom roomId — Caller-supplied id ≤100 chars, unique per app — *match ids are server-generated UUIDs; lobbies have join codes; no caller-supplied match id (`rg short_code` → lobbies only)* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · SuspendRoom / ResumeRoom — Suspend (deprecated May 2024) and resume a room onto a new process — *match snapshots + resumable sessions + replay (R208, migrations match_snapshots) restore state after node loss; no explicit suspend/resume API for dedicated se…* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Meshing vendors (Hadean)** · Client count — "tens of thousands of clients" in one world via distributed networking (Muxer / Hadean Connect edge nodes) — *edge ingress nodes with interest fan-out exist (`musubi-edge`), but no published >10k single-world result* — [source](https://hadean.com/blog/the-spatial-revolution-driving-web-3-0-and-the-metaverse-part-4-networking/)
- 🟡 **Meshing vendors (coherence)** · CoherenceInput — typed inputs Button, Axis/2D/3D, String ≤63, Rotation, Integer; input buffer; `Use Fixed Simulation Frames` — *`world_input{input: json}` untyped; `match_input` tick-stamped for rollback matches* — [source](https://docs.coherence.io/manual/components/coherenceinput)
- 🟡 **Meshing vendors (coherence)** · Floating origin / 64-bit positions — RS stores float64 absolute positions (sub-mm to 5e9 km); per-client `TranslateFloatingOrigin`, `floatingOriginMode` per entity — *positions are f64 (`WorldEntity.x,y`) but 2-D only; no client origin-shift helper* — [source](https://docs.coherence.io/manual/advanced-topics/big-worlds/world-origin-shifting)
- 🟡 **Meshing vendors (Star Citizen)** · Hybrid Service — RL + persistence functions split into a Hybrid Service between client and DGS ("Fixed a Hybrid Service Crash" in 4.0) — *world host node acts as hub (307 redirects, event bus); not a dedicated replication service* — [source](https://starcitizen.tools/Star_Citizen_Alpha_4.0)
- 🟡 **Meshing vendors (coherence)** · Max queries per client — default 15 (`--max-queries-per-client`); total = per-client × clients — *one `world_subscribe` radius per client; no multi-query count/limit concept* — [source](https://docs.coherence.io/manual/replication-server)
- 🟡 **Meshing vendors (Star Citizen)** · Replication Layer separate from game servers — since Alpha 3.18 the entire universe state (characters, ships, items) lives in a Replication Layer; clients and DGS both connect to it — *musubi keeps authoritative state inside cell simulators with standby replicas and journal; the "RL" role is split across world host + Postgres; no independent…* — [source](https://starcitizen.tools/Replication_layer)
- 🟡 **Meshing vendors (Flax Engine high-level networking)** · Roles & ownership — roles None / Owned Authoritative / Replicated / Replicated Simulated; ownership scopes server-only, server+clients, server+owning client, o… — *server-authoritative only; no client-owned replicated objects* — [source](https://docs.flaxengine.com/manual/networking/high-level/replication.html)
- 🟡 **Meshing vendors (coherence)** · Send / receive frequency — `--send-frequency` 20 pps, `--recv-frequency` 60 pps; RS commands clients to throttle; auto throttle under load; per-project tuning in dash… — *world tick 30 Hz, per-client `max_hz` on subscribe (`WorldSubscribe.max_hz`); no server→client "slow down" command or receive-rate throttling of world inputs b…* — [source](https://docs.coherence.io/manual/replication-server)
- 🟡 **Meshing vendors (coherence)** · Simulation frame / network time — `ClientSimulationFrame` 60 Hz synced to RS, `NetworkTimeScale` correction, `ClientFixedSimulationFrame`, `OnFixedNetworkUpdate` never skips — *tick numbers in `WorldDelta.tick`, `time_scale` (dilation); no client clock-sync/network-time API in an SDK* — [source](https://docs.coherence.io/manual/advanced-topics/competitive-games/simulation-frame)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Worker heartbeats & window sizes — heartbeat settings; `Client/ServerUpstream/DownstreamWindowSizeBytes` — *session heartbeat; no per-direction window-size knobs* — [source](https://github.com/spatialos/UnrealGDK/blob/master/CHANGELOG.md)
- 🟡 **Nakama** · Match engine queues: `match.input_queue_size` 128, `call_queue_size` 128, `signal_queue_size` 10, `join_attempt_queue_size` 128, `deferred_queue_size` 128, `join_marker_deadline_ms` 15000, `max_empty_sec` 0, `label_update_interval_ms` 1000 — OSS — *actor runtime has tick/mailbox configs but not these exact knobs* — [source](https://heroiclabs.com/docs/nakama/getting-started/configuration/)
- 🟡 **Photon** · Backgrounding behaviour — documented mobile/WebGL background timeouts; PlayerTTL/EmptyRoomTTL guidance — *session resume/reconnect exists (`rejoin`/`reconnect` in realtime.rs) but no client-side handling (no SDK)* — [source](https://doc.photonengine.com/realtime/current/troubleshooting/known-issues)
- 🟡 **Photon** · EmptyRoomTTL — ms an empty room lives (≤300000 on Cloud); BeforeCloseGame plugin after TTL — *matches end when empty via actor idle timeout; not per-room configurable by client* — [source](https://doc.photonengine.com/server/v5/plugins/manual)
- 🟡 **Photon** · Fusion Statistics 2.1 — in-game overlay (simulation/network/memory pages: forward/resim ticks, objects in/out, RTT, bandwidth); debug DLL only — *server metrics (Prometheus, match tick metrics); no client overlay* — [source](https://doc.photonengine.com/fusion/current/manual/testing-and-tooling/fusion-statistics-2.1)
- 🟡 **Photon** · Instantiation (PUN) — PhotonNetwork.Instantiate/InstantiateRoomObject, scene vs player objects, ViewIDs — *world entity spawn API server-side; no client view IDs* — [source](https://doc.photonengine.com/pun/current/getting-started/feature-overview)
- 🟡 **Photon** · MasterClientId as room property — settable via SetProperties (int/double/byte) and in Create/Join — *relay host designated by server; no client-set master* — [source](https://doc.photonengine.com/server/v5/version-history/photon-5-0-12-release)
- 🟡 **Photon** · Photon Stats GUI / Lag simulation GUI — in-game stats overlay & lag simulation (PUN) — *no client overlay; server metrics + soak impairment* — [source](https://doc.photonengine.com/pun/current/troubleshooting/photon-stats-gui)
- 🟡 **Photon** · Start online game from local snapshot (3.0.5+) — promote local sim to online using serialized frame; secret room — *authoritative match can start from stored snapshot (journal/snapshots); no client-sim promotion* — [source](https://doc.photonengine.com/quantum/current/manual/game-session/starting-from-snapshot)
- 🟡 **Photon** · Traffic stats / support logger / disconnect analysis — client-side traffic counters, SupportLogger, disconnect cause taxonomy and docs — *RTT/ping messages + Prometheus server metrics; no client-side traffic stats SDK (no client SDK)* — [source](https://doc.photonengine.com/realtime/current/troubleshooting/analyzing-disconnects)
- 🟡 **Pragma** · Game-server providers (hosting) — `GameServerProviderPlugin` implementations referenced: AWS GameLift, Hathora, Unity Multiplay, local process; studio writes plugin. — *`musubi-fleet` providers: local, Docker, Kubernetes/Agones, Firecracker, SSH bare-metal, webhook, Edgegap, GameLift. No Hathora or Multiplay adapter (webhook p…* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/in-depth-topics/game-server-in-depth)
- 🟡 **Rivet** · Actor keys (string / compound array keys, injection-safe) — `getOrCreate(["room","general"])`, hierarchical addressing — *matches/worlds addressed by UUID (`match_create`, `world_subscribe`), modules by name; no user-defined compound keys for compute units* — [source](https://rivet.dev/actors/docs/keys/)
- 🟡 **Rivet** · Actor-to-actor calls (`c.client()`), traced end-to-end — typed server-side client inside actors — *modules can call host ops (`musubi.host_call`: storage, wallet, notify…) and `match.*` host ops, but not arbitrary module→module RPC (`rg -i "host_call"`)* — [source](https://rivet.dev/actors/docs/communicating-between-actors/)
- 🟡 **Rivet** · Long-running workers connect outbound (`registry.startEnvoy()`), no public URL / inbound firewall rule — Envoy runner protocol (BARE) — *dedicated game servers register inbound with `X-Musubi-Server-Id` and the SDK heartbeat (README L233); nodes are the compute, no separate worker tier* — [source](https://rivet.dev/docs/workers/)
- 🟡 **Rivet** · Orchestration error codes: `crashed`, `no_capacity`, `runner_no_response`, `runner_connection_lost`, `runner_draining_timeout`, `concurrent_actor_limit_reached` — surfaced in dashboard — *fleet allocation errors (`no capacity`, buffer exhausted) in `fleets.rs`; less granular* — [source](https://rivet.dev/docs/troubleshooting/)
- 🟡 **Rivet** · getOrCreate / get / create / getForId / resolve — Idempotent create-or-get by key; `PUT /actors` create-or-get — *`POST /v1/server/matches` create and `match_join` by id; no idempotent get-or-create by key (`rg -i "get_or_create"` = 0)* — [source](https://rivet.dev/actors/docs/clients/javascript/)
- 🟡 **SpacetimeDB** · Compression — Gzip default (Brotli stubbed) on WS; TS decompression hardening (REL 2.8.1) — *grep shows compression only in snapshot/blob paths; no documented WS payload compression negotiation*
- 🟡 **SpacetimeDB** · Database-is-the-server model — App logic (reducers/procedures/views/HTTP handlers) runs inside the DB process; clients connect directly, no separate game/app server tier — *musubi runs server logic as sandboxed WASM modules (`musubi-runtime`, `POST /v1/rpc/{id}`) and authoritative match/world sims, but data is in Postgres, not an…*
- 🟡 **SpacetimeDB** · HTTP/2 backend — Enabled REL 2.3.0/2.7.1; CORS preflight caching — *musubi axum server + QUIC; HTTP/2 only via reverse proxy; CORS not found in repo (grep 0)*
- 🟡 **SpacetimeDB** · Lifecycle reducers — `init` (first publish / data clear), `client_connected`, `client_disconnected`; error in `client_connected` rejects the connection — *module hooks on match/world lifecycle and `StatusPresenceEvent`; no module-level connect/disconnect hooks for every socket session (grep: no `client_connected`…*
- 🟡 **UGS LiveOps** · Rollout percentage (sticky per user id) — 0–100 %, random assignment by user id — *Module canary `--percent` applies to code, not config; Zanshin flags provide % rollouts when deployed; core has `rg -i rollout` → 0* — [source](https://docs.unity.com/en-us/remote-config/game-overrides-and-settings)

**P2 · dedicated servers/orchestration** — owner: musubi-fleet (`FleetSpec`, `/v1/server/fleets/*`, `/v1/gameserver/*`, providers)

- 🟡 **Agones/OM** · Cluster creation guides: GKE (Standard/Autopilot, release channels), EKS (eksctl), AKS, OKE, minikube (multi-node) — Docs — *Terraform modules assume an existing cluster except Hetzner/OVH (which create VMs); no cloud cluster-creation guides* — [source](https://agones.dev/site/docs/installation/creating-cluster/)
- 🟡 **Agones/OM** · Container OS/arch matrix: linux/amd64 stable, linux/arm64, windows alpha — Installation page table — *Rust binary builds for linux amd64/arm64; no documented arch matrix for game-server images* — [source](https://agones.dev/site/docs/installation/)
- 🟡 **Agones/OM** · Dual-stack / IPv6 load balancer settings (`ipFamilyPolicy`, `ipFamilies`) for ping and allocator services (1.48) — Helm — *IPv6 addresses accepted in addresses (`rg -i ipv6` → 2 hits) but Helm Service has no family settings*
- 🟡 **Agones/OM** · Fleet name ≤ 63 chars; validation webhooks reject bad specs — k8s DNS label rules — *Server-side validation of fleet payload exists (`FleetSpec` validation in fleets.rs); no schema published beyond OpenAPI*
- 🟡 **Agones/OM** · GameServer `kubectl describe` events for each lifecycle transition (`Starting`, `Ready`, `Unhealthy`, …) — Troubleshooting guide — *`GET /v1/server/events` + webhooks deliver server events; no per-object event history* — [source](https://agones.dev/site/docs/guides/troubleshooting/)
- 🟡 **Agones/OM** · Quilkin UDP proxy integration (Embark) for routing/filtering in front of GameServers — Mentioned in system diagram — *musubi has its own UDP/QUIC edge for its realtime transports (README:realtime transports), not a generic DGS proxy* — [source](https://agones.dev/site/docs/advanced/system-diagram/)
- 🟡 **Agones/OM** · Response `status.state` = `Allocated` / `UnAllocated` / `Contention`; `gameServerName`, `address`, `addresses`, `ports[]`, `nodeName`, `source`, `metadata`, `counters`, `lists` — `Contention` on cache races — *Response has server id, address, port, `connect_token`; `wait_secs` long-polls instead of returning UnAllocated; no node/source fields*
- 🟡 **Agones/OM** · `remoteAllocationTimeout`, `totalRemoteAllocationTimeout`, `apiServerQPS/Burst` for the allocator — Helm — *`wait_secs` on the request; no per-service timeout tuning*
- 🟡 **Agones/OM** · `spec.counters{key:{count,capacity}}` declared at creation (default capacity 1000, keys immutable afterwards) — Beta `CountsAndLists`, default on since 1.41 — *Only one implicit counter: `players`/`capacity` on `game_servers` (mig 0067, `POST /v1/gameserver/players`). No user-defined keys, no multiple counters (rooms…*
- 🟡 **Agones/OM** · `status.address` picked by priority ExternalDNS > ExternalIP > InternalDNS > InternalIP; `status.addresses[]` lists all node addresses and Pod IPs (IPv4/IPv6) — Since 1.35 (`addresses`) — *`address` is a single string set by the provider (`public_addr` for ssh, node IP for k8s); no multi-address list, no DNS preference*
- 🟡 **Agones/OM** · `status.nodeName`, `status.reservedUntil`, `status.players`, `status.counters`, `status.lists`, `status.ports[]` — Full status object readable via SDK and API — *`node_id`, `address`, `port`, `players`, `capacity`, `labels`, `allocation`, `build_version`, `ready_at`, `terminated_at`, `version` (mig 0067) — no reservatio…*
- 🟡 **Agones/OM** · `sync.type: FixedInterval` with `seconds` (default 30) — Only sync strategy — *Reconcile cadence fixed at 5 s (fleet/lib.rs), not per fleet*
- 🟡 **Colyseus** · 0.17 full-stack typing: declare `client.auth`/`client.userData` types once on backend, infer room message/payload types in `room.send()`, infer HTTP route request/response types in `client.http.*` (end-to-end TS inference) — 0.17 — *OpenAPI (`/openapi.json`) + IDL codegen give typed contracts, but no single-language end-to-end inference (server is Rust/WASM, client is anything)* — [source](https://docs.colyseus.io/roadmap)
- 🟡 **Colyseus** · Pluggable `logger` (pino/winston), `greet` banner toggle — *`tracing` structured logs + OpenTelemetry; not pluggable to arbitrary JS loggers (n/a in Rust)* — [source](https://docs.colyseus.io/server/logging)
- 🟡 **Colyseus** · Presence API: `LocalPresence` (default) / `RedisPresence` (ioredis, Redis Cluster): pub/sub `subscribe/unsubscribe/publish/channels`; KV `set/setex/expire/get/del/exists/incr/decr`; sets `sadd/smembers/sismember/srem/scard/sinter`; hashes `hset/hget/hgetall/hdel/hlen/hincrby`; lists `llen/rpush/lpush/rpop/lpop/brpop` — Redis for multi-process — *modules get `kv` host ops (get/set/cas/ttl via Postgres) + cluster event bus; no Redis-like set/hash/list/blocking primitives exposed to modules* — [source](https://docs.colyseus.io/server/presence)
- 🟡 **Colyseus** · Transport: WebSocket default (`ws`) with `pingInterval` (3000 ms) / `pingMaxRetries` (2) heartbeat, `maxPayload` 4 KB, `perMessageDeflate`, `verifyClient`, `beforeUpgrade(Request, ctx{token,headers,ip})` — ws transport options — *WS `GET /ws?token=&format=json\|binary`, ping/pong + `rtt`, `[anticheat] max_input_bytes`, lz4 per-frame instead of deflate; no pre-upgrade HTTP hook (token au…* — [source](https://docs.colyseus.io/server/transport/ws)
- 🟡 **Colyseus** · Type-safe HTTP routes `createRouter()/createEndpoint()` (shared types to SDK) + classic `express` callback for arbitrary routes/middleware — built-in HTTP — *custom server logic exposed only as `POST /v1/rpc/{id}` (WASM, typed via IDL `[rpcs.*]` → OpenAPI); no arbitrary HTTP routes/Express middleware* — [source](https://docs.colyseus.io/server/http-routes)
- 🟡 **Colyseus** · Typed HTTP routes `createEndpoint/createRouter/createMiddleware` (ctx.context merge), `basicAuth` middleware (401 + WWW-Authenticate), JWT `auth.middleware()` (revocation-aware with database), Express routes, CORS on by default (customizable), frontend `client.http.*` — *custom logic exposed via WASM `rpc` exports (`POST /v1/rpc/{id}`) with OpenAPI; no arbitrary HTTP path/verb routing, no middleware chain; CORS configurable in…* — [source](https://docs.colyseus.io/server/http-routes)
- 🟡 **Colyseus** · `defineRoom(RoomClass, defaultOptions)` — same class under several names with server-side option override — *`POST /v1/server/matches {module, params}` / IDL `[matches.<module>] params` validation; no named presets of one module* — [source](https://docs.colyseus.io/server)
- 🟡 **Edgegap** · Deploy to fleet API — `POST /v2/private-fleets/deployments` with `private_host_ids` priority and cloud overflow; `GET /v2/private-fleets/{fleet-name}/hosts` (act… — *`provider = ssh` fleets with host list; no overflow to a second provider* — [source](https://docs.edgegap.com/docs/api/dedicated-servers)
- 🟡 **Edgegap** · Deployment "users" telemetry — Player IPs counted on dashboard as total players; per-request `users` list — *Allocation carries `payload` (opaque); players reported by server via `/v1/gameserver/players`* — [source](https://docs.edgegap.com/docs/api/dedicated-servers)
- 🟡 **Edgegap** · Deployment Agent sample (stop + read env from server) — Unity sample for projects without matchmaker/SB — *Game-server SDK is HTTP only (no engine sample)* — [source](https://docs.edgegap.com/docs/release-notes/archive)
- 🟡 **Edgegap** · External registry guides — AWS ECR, Docker Hub, GCP GCR, GitLab registry credential setup — *Docker/k8s providers accept any image but musubi stores no pull credentials* — [source](https://docs.edgegap.com/docs/tools-and-integrations/docker/external-registries)
- 🟡 **Edgegap** · Image source: Edgegap registry / Docker Hub / private registry profile with credentials — Registry profile stores host, repo, username, token; auto-fetch of available images/tags when creating a version — *`spec.image` is a plain reference passed to docker/k8s/edgegap provider; no stored registry credential profiles or image/tag discovery (`rg registry` finds onl…* — [source](https://docs.edgegap.com/learn/orchestration/application-and-versions)
- 🟡 **Edgegap** · Pull profiles (files from S3 into container at start) — source file/folder → destination path; linked to versions; ≤5 MB total; API only — *Content bundles / storage objects exist for players; no file injection into game-server containers* — [source](https://docs.edgegap.com/docs/endpoint-storage/upload-files)
- 🟡 **Edgegap** · Session configuration on version — Kind Default/Seat/Match, `sockets` capacity, `autodeploy`, `empty_ttl`, `session_max_duration` — *`GameServer.capacity/players`, backfill allocation; no socket-accounting session objects per deployment (see Sessions rows)* — [source](https://api.edgegap.com/swagger.json)
- 🟡 **Edgegap** · Stop deployment / bulk stop — `DELETE /v1/stop/{request_id}`, `POST /v1/deployments/bulk-stop` with filters (tags, app, version, status); `container_log_storage` on stop — *`DELETE /v1/server/gameservers/{id}`, `POST /v1/server/fleets/{id}/scale`; no filter-based bulk stop* — [source](https://docs.edgegap.com/docs/api/dedicated-servers)
- 🟡 **GameLift** · Anywhere fleets (customer hardware) — `CreateLocation custom-*`, `CreateFleet ComputeType ANYWHERE`, `RegisterCompute{ComputeName, IpAddress, DnsName, Location}`, `GetComputeAut… — *`ssh` provider (bare metal), webhook/external providers; no self-registering compute API with auth tokens* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-intro-anywhere.html)
- 🟡 **GameLift** · Anywhere roadmap & creation — Create custom location → fleet → register compute → auth token → start server — *ssh provider automates; no registration flow for externally-started servers except `POST /v1/gameserver/ready` with server key* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-creating-anywhere.html)
- 🟡 **GameLift** · Build object & console — `Build{BuildId, Name, Version, Status INITIALIZED/READY/FAILED, SizeOnDisk, OperatingSystem, CreationTime, ServerSdkVersion}`; console buil… — *`FleetBuild{version, spec, state, canary_percent, notes, created_at}`; no binary/size/OS* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-console-builds.html)
- 🟡 **GameLift** · Containers starter kit / wrapper — Toolkit starter kit; game-server wrapper to run unmodified servers — *`FleetSpec.command` runs any binary but it must call `/v1/gameserver/ready` to serve* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-wrapper-tutorial.html)
- 🟡 **GameLift** · Edit fleet attributes — `UpdateFleetAttributes{Name, Description, NewGameSessionProtectionPolicy, ResourceCreationLimitPolicy, MetricGroups, AnywhereConfiguration.… — *`POST /v1/server/fleets/{id}/scale` + build updates; name/labels editable, no protection/limit policies* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-editing.html)
- 🟡 **GameLift** · Fast build update / multi-build tooling — Toolkit: fast-build-update-tool, multi-build-solution — *versioned builds + canary per fleet, but no binary distribution* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-wrapper-tutorial.html)
- 🟡 **GameLift** · Fleet creation lifecycle — States NEW→DOWNLOADING→VALIDATING→BUILDING→ACTIVATING→ACTIVE (ERROR/DELETING); install-script events; remote access allowed during ACTIVATI… — *fleets are `active` immediately, builds have `state` (`FleetBuild.state`), no multi-phase provisioning events* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-creating.html)
- 🟡 **GameLift** · Fleet metric groups — Aggregate CloudWatch metrics across fleets via `MetricGroups` (1 per fleet) — *Prometheus labels by fleet/region/pool; no explicit metric groups* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_FleetAttributes.html)
- 🟡 **GameLift** · Fleet port settings — `IpPermission` TCP/UDP 1–60000, CIDR; `UpdateFleetPortSettings`; max 50 ranges — *`port_start/port_end` per fleet; no CIDR allow-list on fleet* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_UpdateFleetPortSettings.html)
- 🟡 **GameLift** · Game session protection — `NewGameSessionProtectionPolicy NoProtection/FullProtection` – protected sessions survive scale-down; per-session override — *reconciler reaps only idle/unhealthy servers (`musubi-fleet` reconcile every 5 s); no explicit per-session protection flag* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-editing.html)
- 🟡 **GameLift** · Getting started examples — Sample game servers (C++/C#) — *examples in repo `examples/`? not verified* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/getting-started-examples.html)
- 🟡 **GameLift** · Windows game servers — Windows Server 2016/2022 fleets, RDP access — *`local`/`ssh` providers can run Windows binaries? not verified → partial; docker/k8s providers Linux* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-remote-access.html)
- 🟡 **Hathora** · Create room against a specific deployment — `deploymentId` on CreateRoom (Nov 2024) — pin a room to an older/newer deployment — *canary share by percent/region picks build at server start; cannot pin a single allocation to a specific build* — [source](https://blog.hathora.dev/changelogs-november-2024/)
- 🟡 **Hathora** · Eager build caching / baked node images — Pro tier: images pre-pulled to nodes; May 2025 40 % faster node boot — *`buffer` ready pool hides cold starts; no image pre-pull orchestration (provider-dependent)* — [source](https://blog.hathora.dev/new-pricing-tiers-for-every-stage-of-development/)
- 🟡 **Hathora** · Global builds (shared across apps) — Build once, deploy to dev/staging/prod apps; promote/rollback between apps with one command (Sep 2024) — *promote/rollback within a fleet (R194); same image can be referenced by several fleets, but no cross-"app" build object* — [source](https://blog.hathora.dev/global-builds/)
- 🟡 **Hathora** · Private backbone / edge routing — Traffic through Hathora premium edge network; `*.edge.hathora.dev` ingress; latency-aware load balancers with failover — *UDP edge proxy + georouting exist in-cluster; no global anycast backbone (needs operator's CDN/network)* — [source](https://hathora.dev/docs/faq/reduce-latency)
- 🟡 **Hathora** · Process object — id, appId, deploymentId, buildId, fleetId, region, hosting (metal/cloud/shared), status, roomsAllocated, roomsPerProcess, exposedPort(s), t… — *game server rows have fleet/region/state/build_version/labels/ready_at/terminated_at (R194); no exit-reason summary, no hosting class (`rg exit_reason` → none)* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **LootLocker** · Inventory server ops: Alter Player Inventory (grant/remove), character & hero inventories, universal assets, simplified inventory listing — *`/v1/server/inventory/grant`, consume, instances; no character/hero-scoped inventories* — [source](https://ref.lootlocker.com/server)
- 🟡 **Metaplay** · Database scan jobs (batch wake all entities of a type for migrations/maintenance; dashboard progress) — *retention/cleanup jobs + storage migrations run server-side; no generic entity-scan job framework with UI* — [source](https://docs.metaplay.io/game-server-programming/how-to-guides/database-scan-jobs)
- 🟡 **Metaplay** · Queuing server actions to offline players (executed when actor wakes); custom entities how-to; entity-to-entity communication deep dive; troubleshooting servers — *WASM hooks + scheduled logic + deferred rewards; no "queue action against offline player model" primitive* — [source](https://docs.metaplay.io/game-server-programming/how-to-guides/queuing-server-actions)
- 🟡 **Photon** · Multi-Peer mode — multiple NetworkRunners per Unity instance (editor multi-client testing, multi-session dedicated servers), runner visibility — *`musubi bench`/`soak` spawn many simulated clients; multi-match per node is native (actors)* — [source](https://doc.photonengine.com/fusion/current/manual/testing-and-tooling/multipeer)
- 🟡 **Photon** · Policy file listener, S2S, HTTP routing — Flash policy, server-to-server, HTTP listener routes — *internal node bus (Postgres LISTEN/NOTIFY, SWIM) instead of S2S config* — [source](https://doc.photonengine.com/server/v5/operations/server-config-settings)
- 🟡 **PlayFab** · Assets & certs APIs: GetAssetUploadUrl, GetAssetDownloadUrl, ListAssetSummaries, DeleteAsset, UploadCertificate, ListCertificateSummaries, DeleteCertificate, ListContainerImages, ListContainerImageTags (PageSize/SkipToken), UntagContainerImage, RolloverContainerRegistryCredentials, GetContainerRegistryCredentials — Per-title ACR free — *Builds reference container images/artifacts you host; no asset upload/cert store* — [source](https://learn.microsoft.com/rest/api/playfab/multiplayer/multiplayer-server/)
- 🟡 **Rivet** · Backups: `pg_dump`, stop-and-tar RocksDB, FDB backup agent (EE), restore test checklist — SQLite backup + PITR Enterprise — *Postgres-native backups implied; `musubi export`/`import` for data (`transfer.rs`) and per-account backup/restore; no documented backup runbook or PITR (`rg -i…* — [source](https://rivet.dev/docs/deploy/self-host/control-plane/backups/)
- 🟡 **Rivet** · Storage backends: filesystem/RocksDB (single node), PostgreSQL (multi-node, ~1,000 concurrent actors, no PgBouncer/poolers, SSL + client certs), FoundationDB (Enterprise, linear scaling) — BYO DB since v2.0 — *Postgres only (+ Raft in `musubi-cluster` for metadata); no embedded single-node store and no FDB-class scale-out* — [source](https://rivet.dev/docs/deploy/self-host/control-plane/storage/)
- 🟡 **SpacetimeDB** · Backups / PITR — Pro: automatic replication + backups, 7-day PITR; Team: 30-day PITR; standalone: copy `/stdb` data dir / commitlog — *no built-in backup tooling (grep `backup`, `pitr` = 0 hits); relies on Postgres backups; `musubi export/import` JSONL dumps*
- 🟡 **Steamworks** · Secure (VAC) servers — `BSecure()`, `GSPolicyResponse_t`; VAC-banned players denied — *anti-cheat signals + EAC/BattlEye ticket verification + automatic kicks (README Anti-cheat); no Valve-run kernel/user-mode anti-cheat* — [source](https://partner.steamgames.com/doc/api/ISteamGameServer)
- 🟡 **UGS Multiplayer** · API: image jobs (install, update, difference analysis, rollout status, version status, options) — *Build versions + canary/promote/rollback; no image job pipeline.* — [source](https://support.multiplay.dev/articles/4951324689-differential-updates)
- 🟡 **UGS Multiplayer** · Auto scaling / capacity failure handling — Unity scales Relay transparently at no cost; no cross-region reroute on capacity failure — client retries — *Nodes scale via Helm/Terraform + cluster membership; relayed-match capacity is your node count (no managed autoscale of relay nodes).* — [source](https://docs.unity.com/en-us/mps-sdk/networking/scaling)
- 🟡 **UGS Multiplayer** · Builds from S3 / GCS; releases — upload loose files (no zip) from S3/GCS bucket or CLI; Release N versions, only latest usable — *Fleet builds are versioned `spec` (image/command/env/vcpu) via `musubi fleet push` (README); no object-storage ingestion of raw files.* — [source](https://support.multiplay.dev/articles/5527863016-builds-overview)
- 🟡 **UGS Multiplayer** · Machine report / server start-stop-restart-save actions — Clanforge API `/cfp/v1/server/{id}/save / restart`, machine report — *Server stop/scale via fleet routes; no "save" action.* — [source](https://support.multiplay.dev/articles/3470932610-get-machine-report)
- 🟡 **UGS Multiplayer** · Rollout modes progressive (empty servers only) vs forced (kick) — build update triggers installs — *Build `canary_percent`/`canary_regions` → `promote`/`rollback` (README 5.x) gives staged rollout; servers drain by allocation; no explicit "forced kick" mode.* — [source](https://support.multiplay.dev/articles/5527863016-builds-overview)
- 🟡 **UGS Multiplayer** · Test allocations from dashboard — select fleet/region/build config; no payload — *API allocate works for tests; no UI.* — [source](https://support.multiplay.dev/articles/1969740048-manage-test-allocations)

**P2 · meshing/world** — owner: musubi-mesh + musubi-sim-sdk (`/v1/server/worlds/{w}/*`, realtime `world_*`)

- 🟡 **Meshing vendors (coherence)** · Authority models — client authority, server authority, server authority + client input/prediction, distributed (multi-sim), mixed per entity — *server/simulator authority only (lease holder writes); clients send `world_input`; no client-authoritative entity mode* — [source](https://docs.coherence.io/manual/networking-state-changes/authority)
- 🟡 **Meshing vendors (coherence)** · Commands (RPC) — bound public methods, routing modes, `MessageTarget` AuthorityOnly/Other/All/InputAuthorityOnly, generated extension methods, `SendOrderedC… — *`interact` (seq-ordered, routed to owner) and `POST /v1/rpc/{id}` server RPCs; no client→client/all-clients command fan-out or generated typed stubs* — [source](https://docs.coherence.io/manual/networking-state-changes/commands)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Component delivery / filters — `checkoutAllInitially`, per-component override, `RELIABLE_ORDERED` default, static + dynamic (runtime) component filters — *subscribe-time `filter` only; no per-component reliability class* — [source](https://documentation.improbable.io/spatialos-overview/docs/component-delivery)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Design guides — inventory systems, shooting/hit detection (client detect + server authority + validation), object interaction, third-party integration, lau… — *README/parity only; no design guides* — [source](https://documentation.improbable.io/spatialos-overview/docs/design-guides)
- 🟡 **Meshing vendors (Hadean)** · Distributed debugging — "Future release: Distributed Debugging" — *per-world history/time-machine + request record/replay, no live distributed debugger* — [source](https://web.archive.org/web/20200215103741/https://www.hadean.com/aether-engine/features)
- 🟡 **Meshing vendors (coherence)** · Engine support — Unity only (verified Win/macOS/Linux/Android/iOS/WebGL); Unreal "no public tools", partner-only; custom engines via devrel — *no Unity SDK; Unreal scaffold not CI-compiled; Rust sim SDK only* — [source](https://docs.coherence.io/support/unreal-engine-support)
- 🟡 **Meshing vendors (Star Citizen)** · Entity Component Update Scheduler — skip updates of individual components too far from player — *per-client component filter and relevance ranking, but no distance-based per-component rate* — [source](https://starcitizen.tools/Object_Container_Streaming)
- 🟡 **Meshing vendors (Star Citizen)** · Entity Graph — graph database of entities and relationships (Entity Ownership Hierarchy, Entity Aggregates) — *attach trees (root/passengers) only* — [source](https://starcitizen.tools/Star_Engine)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · EntityTemplate / ACL write access — components added with write-access worker attribute sets — *authority = lease holder; no per-component write ACL* — [source](https://github.com/spatialos/gdk-for-unity/blob/master/CHANGELOG.md)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Interest — `AlwaysInterested` properties, `bOnlyRelevantToOwner`, NetCullDistance → interest radius, owner interest — *radius+filter+budget; no always-interested / owner-only flags* — [source](https://github.com/spatialos/UnrealGDK/blob/master/CHANGELOG.md)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Multi-worker debugging — Visual Logger & Gameplay Debugger across workers, Unreal Insights `StopInsights`, local/cloud log levels — *OTLP tracing + per-world history; no engine debugger integration* — [source](https://github.com/spatialos/UnrealGDK/blob/master/CHANGELOG.md)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Network Analyzer — commands/world-commands bandwidth tabs — *Prometheus fan-out metrics only* — [source](https://github.com/spatialos/gdk-for-unity/blob/master/CHANGELOG.md)
- 🟡 **Meshing vendors (coherence)** · Per-field sampling frequency at runtime — sample rate per binding adjustable at runtime; `SyncMode.Manual` + `MarkForSyncing()` (2.1) — *per-client `max_hz` only; no per-component rate* — [source](https://docs.coherence.io/overview/features)
- 🟡 **Meshing vendors (Flax Engine high-level networking)** · Replication hierarchy — custom `NetworkReplicationHierarchy` with per-object `ReplicationFPS`, relevancy/distance culling for e.g. 10k POIs, `DirtyObject` — *per-client budget/relevance ranking; no per-object replication rate* — [source](https://docs.flaxengine.com/manual/networking/high-level/replication.html)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Schema language — schemalang components/events/commands, generated APIs, backward-compatible evolution (rename/remove/add option/list/map) — *TOML IDL for API/match types; world components untyped* — [source](https://documentation.improbable.io/sdks-and-data/docs/backward-compatibility)
- 🟡 **Meshing vendors (Star Citizen)** · Server frame scale — up to 64 HW threads, 200 SW threads, >700k streamed-in entities per server, ~150k component updates/frame — *30 Hz world ticks per thread; soak at 10k entities per world* — [source](https://starcitizen.tools/Star_Engine)
- 🟡 **Meshing vendors (coherence)** · Simulator build & upload — Hub "Build And Upload Simulator", `SimulatorBuildPipeline.PrepareBuild/BuildAndUpload` CLI, Mono/IL2CPP, asset stripping (experimental), si… — *`musubi fleet` builds canary/promote/rollback; no Unity build pipeline integration* — [source](https://docs.coherence.io/manual/simulation-server/build-deploy-simulators)
- 🟡 **Meshing vendors (coherence)** · State vs Input authority split — separate State authority and Input authority; `TransferAuthority(clientId, AuthorityType.Input)`; `OnInputAuthority/OnInputRemote` — *input routed to authoritative sim (`world_input{entity}`), but no explicit per-entity input-authority assignment API* — [source](https://docs.coherence.io/manual/networking-state-changes/authority)
- 🟡 **Meshing vendors (coherence)** · Transfer types — Not transferable / Request (owner approves via `OnAuthorityRequested`, `Approve Requests` fallback) / Steal (default); `RequestAuthorityAsy… — *handover is cell/placement-driven (`handover/ack`, co-locate pin); no client request/steal semantics* — [source](https://docs.coherence.io/manual/networking-state-changes/authority/authority-transfer)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Transform Synchronization — `CompressedQuaternion`, `FixedPointVector` — *`pos_quantum` only* — [source](https://github.com/spatialos/gdk-for-unity/blob/master/CHANGELOG.md)
- 🟡 **Meshing vendors (coherence)** · Uniqueness — Unique ID (manual or prefab-instance), No Duplicates, Replace/Destroy strategy — *entity UUIDs; idempotent spawn by id in tx; no named-singleton replace semantics* — [source](https://docs.coherence.io/manual/networking-state-changes/uniqueness)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Write access authority per component — exactly one worker writes a component; `EntityAcl.component_write_acl` per component + `read_acl` per entity (worker requirement sets) — *authority is per entity (lease holder writes all components); no per-component ACL* — [source](https://documentation.improbable.io/spatialos-overview/docs/write-access-authority)
- 🟡 **Photon** · NetworkTransform — local-space TRS, optional scale, optional parent sync, AreaOfInterestOverride for nested objects — *world entity transform replication in mesh; nesting/parent sync absent* — [source](https://doc.photonengine.com/fusion/current/getting-started/release-notes/whats-new-2-0)

**P2 · economy/IAP** — owner: musubi-economy (`/v1/store*`, `/v1/iap/*`, `/v1/inventory*`, `/v1/server/rewards/*`)

- 🟡 **Beamable** · Cloud Save Portal data management — Search player data, download object, upload replacement; no historic restore — *Admin storage routes exist; no UI; musubi has storage history/versions (ahead on restore)* — [source](https://docs.beamable.com/docs/cloud-save)
- 🟡 **Beamable** · Commerce `stats-update` & purchase status — Update stats on purchase, clear status — *Purchase hooks via modules* — [source](https://docs.beamable.com/reference/commerce-overview)
- 🟡 **Beamable** · Content snapshots in Git / CLI snapshot & restore — Shared content snapshots, On Publish Auto Snapshot (Shared Only), `.beamable/local/` ignored, Restore Snapshot — *Content bundle versions + export/import to files; no snapshot/restore CLI workflow* — [source](https://docs.beamable.com/docs/content-team-workflows)
- 🟡 **brainCloud** · Platform ID catalogue — 20+ platform ids incl. VISION_OS, WATCH_OS, APPLE_TV_OS, NINTENDO, PS5, XBOX, ROBLOX — *platforms tracked for store/entitlements/crossplay, not a canonical client platform enum* — [source](https://docs.braincloudservers.com/api/appendix/platformids)
- 🟡 **Colyseus** · Lifecycle hooks: onCreate, onAuth (instance or static, ctx token/headers/ip), onJoin(client, options, auth), onDrop (0.17), onReconnect (0.17), onLeave(client, code), onDispose, onUncaughtException(err, methodName), onBeforePatch(state), onCacheRoom/onRestoreRoom (devMode), onBeforeShutdown — all async-capable — *WASM exports init/join_attempt/join/leave/tick/terminate; disconnect-vs-leave is handled by resumable sessions server-side; no onBeforePatch (typed deltas are…* — [source](https://docs.colyseus.io/room/lifecycle)
- 🟡 **Metaplay** · Config items/libraries/archives model: `#key` columns (compound keys), one sheet per library, binary `.mpa` archives as atomic CDN unit, content-hash version id — *musubi content bundles (`/v1/admin/content/*`: versions, diff, rollback, environments, dry-run, prune) are JSON documents, no typed item/library schema or bina…* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/working-with-game-configs)
- 🟡 **Metaplay** · Side-by-side diff of any two versions, search by item id, swap baseline; "Diff to Active" — R34 revamped diffing — *`GET /v1/admin/content/diff` JSON diff only* — [source](https://docs.metaplay.io/feature-cookbooks/game-configs/managing-game-configs-from-the-dashboard)
- 🟡 **Hiro/Satori** · Disabled items hidden from all APIs; keep_zero — Config flags — *item `enabled` flag exists; no keep-zero (show owned-before items at 0) semantics* — [source](https://heroiclabs.com/docs/hiro/concepts/inventory/)
- 🟡 **Hiro/Satori** · Item sets in rewards (set intersection, max_repeats, only items available to player) — `item_sets: [{set:[tags], min,max,max_repeats}]` picks random items tagged with all listed sets — *musubi reward entries reference items/currency/xp/tables; items have `tags`/category but rg "item_set" -> none: no tag-intersection random pick inside a reward* — [source](https://heroiclabs.com/docs/hiro/concepts/economy/rewards/)
- 🟡 **Hiro/Satori** · Purchase restore (`ECONOMY_PURCHASE_RESTORE`) and placement status/success/fail RPCs — Client RPC surface — *musubi `GET /v1/iap/receipts` lists validated receipts for restore; no placements* — [source](https://github.com/heroiclabs/hiro/blob/master/hiro-openapi.yml)
- 🟡 **Hiro/Satori** · Reward destinations: player / mailbox / team / team mailbox / team members — `to_mailbox_expiry_sec` routes to reward mailbox with expiry; `member_reward` for each team member; `message` text on mailbox reward; combi… — *README "Deferred rewards": `deferred: {delay_secs, ttl_secs, title, body}` freezes outcome into mail with reward_entries; group treasury/inventory exist but no…* — [source](https://heroiclabs.com/docs/hiro/concepts/economy/rewards/)
- 🟡 **Hiro/Satori** · `SetConfigSource` per-item fallback lookup for items unknown at deploy time (procedural / external) — Runtime catalogue extension — *musubi items are DB rows creatable at runtime via `POST /v1/server/economy/items`; no lazy resolver hook* — [source](https://heroiclabs.com/docs/hiro/concepts/inventory/)
- 🟡 **PlayFab** · Catalog APIs: CreateDraftItem, UpdateDraftItem, GetDraftItem(s), DeleteItem, PublishDraftItem, GetItem(s), GetItemPublishStatus, GetEntityDraftItems, GetEntityItemReview, GetItemContainers, SetItemModerationState, SubmitItemReviewVote, TakedownItemReviews, UpdateCatalogConfig / GetCatalogConfig, CreateUploadUrls, SearchItems, GetItemReviews / GetItemReviewSummary, ReportItem, ReviewItem — Entity API /Catalog — *README items/store admin routes (`/v1/server/items`, `/v1/store`) — no draft/publish, no review votes/takedown* — [source](https://learn.microsoft.com/rest/api/playfab/economy/catalog/)
- 🟡 **Pragma** · Content Data catalogs — Protobuf-defined content types + JSON catalogs + Kotlin `ContentHandler` (load/validate/access); `pragma content apply`; versioned in Git,… — *content bundles/environments (`GET/POST /v1/admin/content`, README "Content bundles") cover definition tables (currencies, items, rewards, quests, store, …) an…* — [source](https://pragma.gg/docs/2026.3.0/concepts/game-data/content-data-key-concepts)
- 🟡 **Pragma** · Player order history (operator) — Game Operator Portal → Player Support shows orders/fulfillments per player. — *admin routes list receipts/ledger per account; no UI.* — [source](https://pragma.gg/docs/2026.3.0/concepts/commerce/how-to-guides/view-players-order-history)
- 🟡 **Steamworks** · Promo items — `promo: "owns:APPID;played:APPID/mins;manual"`, `AddPromoItem(s)`/`GrantPromoItems`, `granted_manually`, eligible promo defs query — *entitlement-driven grants (Steam/PSN/Xbox ownership sync → entitlements) can gate rewards; no declarative "owns other app" promo rule engine* — [source](https://partner.steamgames.com/doc/features/inventory/schema)
- 🟡 **UGS LiveOps** · Editor / CLI / dashboard / REST authoring + deployment of economy resources — `.ec` assets, `ugs deploy` — *`musubi deploy` manifest + content bundles; no editor integration* — [source](https://docs.unity.com/en-us/economy/write-configuration)

**P2 · progression/LiveOps** — owner: musubi-economy progression/quests/seasons + `/v1/server/audiences`, `/v1/liveops/*` (Zanshin)

- 🟡 **AccelByte** · Goal assignment customization via Extend override (e.g. by player level) — Extend — *generic WASM hooks exist but no quest/goal-assignment hook point (rg `quest.*hook` none)* — [source](https://docs.accelbyte.io/gaming-services/modules/online/challenges/goals-assignment-customization/)
- 🟡 **Beamable** · Events: rules, rewards, event stores, group events — Optional rules (name/value, client-parsed), rewards by score or rank (currency/items), event-scoped limited store id, group rewards (must j… — *Live events (Zanshin/liveops host API) + store windows + audiences; no event-attached store or group-event reward engine* — [source](https://docs.beamable.com/docs/events-overview)
- 🟡 **Colyseus** · Message input validation with Zod `validate()` — failing message disconnects client with 4002 (or rejects the request) — *IDL types validate `/v1/rpc/{name}` input and `[matches.*] params/state`; per-message validation inside a match is the module's job* — [source](https://docs.colyseus.io/room)
- 🟡 **Colyseus** · Request/response: handler return value answers `room.request()` / `room.send(type, payload, cb)`; `ctx.reject(reason)`; promise rejection propagates — *socket `rpc` envelope `{cid, msg}` gives request/response for RPCs; no reply-to for in-match messages* — [source](https://docs.colyseus.io/room)
- 🟡 **GameLift** · TLS for service API — TLS 1.2 minimum, 1.3 recommended; SigV4 request signing; FIPS endpoints — *HTTPS at edge, API keys/JWT; no FIPS mode (`rg -i fips` → none)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/data-protection.html)
- 🟡 **LootLocker** · Base Classes: name, equippable Contexts, default loadout slots; Default Character Class mandatory; Context settings (name, friendly name, detachable, complexity, allowed classes, user-facing for Steam inventory, Depends On feature unlock) — *Item equip/unequip slots (`/v1/inventory/equip`) and item kinds exist; no class archetypes with default loadouts, no feature-unlock dependency between slots* — [source](https://docs.lootlocker.com/game-systems/classes-and-heroes/how-to/base-classes)
- 🟡 **LootLocker** · Onboarding record `ll.onboarding.<playtest>` with document content_hash + acknowledgement timestamps (consent audit) — *Age gate / parental consent records exist (`0107_age_gate.sql`); no generic document-acknowledgement ledger* — [source](https://docs.lootlocker.com/players/surveys/how-to/view-survey-answers)
- 🟡 **Hiro/Satori** · API keys (client) vs server keys (console API); users management in console; General settings (UTC display) — Settings — *musubi keys with roles via `POST /v1/admin/keys`; operator accounts/SSO* — [source](https://heroiclabs.com/docs/satori/concepts/settings/)
- 🟡 **Hiro/Satori** · Audit log with Undo of supported console actions — Settings > Audit log — *musubi audit log (0026_audit) records; content versions have rollback, generic actions don't* — [source](https://heroiclabs.com/docs/satori/concepts/settings/)
- 🟡 **Hiro/Satori** · Custom properties registered in Taxonomy with schema; read-only (server/console-only) flag — Client can set non-read-only ones — *account `metadata` JSON + storage; no taxonomy/schema or per-key client write protection (storage has permissions per object)* — [source](https://heroiclabs.com/docs/satori/concepts/segmentation/understand-player-properties/)
- 🟡 **Hiro/Satori** · Data management: import/export via file, import from another Satori server (add server), delete data, sample data, default data — Environments sync — *musubi content bundle export/import between environments (dry_run, prune); fixtures for sample accounts* — [source](https://heroiclabs.com/docs/satori/concepts/data-management/)
- 🟡 **Hiro/Satori** · Get returns delta of changed nodes when previous graph passed; multiple local counts per node; programmatic reset; purchase node — 1.3–1.9 — *musubi graphs: claim/skip/unlock/reset per node, rank-based; no delta diff API* — [source](https://heroiclabs.com/docs/hiro/concepts/progression/)
- 🟡 **Photon** · Ownership & authority (PUN) — PhotonView ownership Fixed/Takeover/Request transfer — *world entity ownership & handover across cells (`musubi-mesh`); no client-side view ownership model (no SDK)* — [source](https://doc.photonengine.com/pun/current/gameplay/ownershipandcontrol)
- 🟡 **Photon** · Ownership transfer modes — Fixed / Takeover / Request — *world entity ownership handover server-side* — [source](https://doc.photonengine.com/pun/current/gameplay/ownershipandcontrol)
- 🟡 **Photon** · Shared Authority mode — per-object State Authority, request/release authority, Photon Cloud server plugin keeps state cache for late-join snapshots; tick ≤32 Hz — *relay match state + world entity ownership/handover; no per-object authority request protocol in client lib* — [source](https://doc.photonengine.com/fusion/current/manual/network-topologies)
- 🟡 **Photon** · Shared Mode Master Client — lowest actor; SetMasterClient; AllowMasterClientReassignRequest + 2 s threshold (2.1) for unresponsive master — *relay host designation; no unresponsive-host client vote* — [source](https://doc.photonengine.com/fusion/current/manual/shared-mode-master-client)
- 🟡 **Pragma** · Persistent EndGame via SQS — API Gateway + SQS + DLQ: EndGame requests durable across restarts; visibility 5 min, maxReceiveCount 100, retention 4 d / DLQ 14 d; idempot… — *end-game reports are persisted in Postgres with idempotent `POST /v1/server/matches/{id}/end`; no external durable queue/DLQ for the ingestion path (webhooks h…* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/in-depth-topics/sqs-end-game)
- 🟡 **Rivet** · Actor primitive — Durable, addressable, schedulable process: state + SQLite + queue + realtime connections + schedules; created on demand, hibernates when id… — *musubi has `musubi-actor` (authoritative match actors: tick loop, snapshots, journal, failover, rollback — README "Authoritative matches") and WASM modules, bu…* — [source](https://rivet.dev/docs/architecture/)
- 🟡 **Rivet** · Low-level HTTP handler (`onRequest`, WinterTC Request/Response, Hono inside actor) — any route under actor — *`POST /v1/rpc/{id}` only; no arbitrary HTTP routing into a module* — [source](https://rivet.dev/actors/docs/request-handler/)
- 🟡 **Rivet** · Queue-driven workflows: `queue.next` waits, request/response, drains, signals, human-in-the-loop approvals — docs — *`POST …/{id}/signal {payload}` wakes a parked workflow (README L422); no typed queue integration* — [source](https://rivet.dev/workflows/docs/queues/)
- 🟡 **UGS LiveOps** · Schedule configs: one-time, recurring (cron), interval (every N units) with optional firing window — Payload ≤1 KB; ≤900 active schedules; 25,000 invocations/month included; name ≤255 unique; delete expired schedules in bulk — *Cron `schedule_*` module exports (`GET /v1/admin/runtime/schedules`, `/run`) are declared in code; durable one-shot timers `timer.schedule` (`/v1/admin/runtime…* — [source](https://docs.unity.com/en-us/triggers/tutorials/schedule-events/unity-dashboard)

**P2 · moderation/anti-cheat** — owner: musubi-server bans.rs/anticheat.rs/automod.rs (`/v1/admin/accounts/{id}/ban`, `/v1/reports`)

- 🟡 **Beamable** · Platform security overview — OAuth2 grants (password, auth code, federated, anonymous); server auth = MD5 signed requests; BCrypt passwords; TLS in flight & at rest; de… — *Argon2/bcrypt pw, JWT rotation, GDPR export/delete routes; backups/VPC are operator responsibility (self-hosted)* — [source](https://docs.beamable.com/docs/security-overview)
- 🟡 **Edgegap** · Secrets encrypted at rest (secret env vars, S3 secret, registry token) — Documented encryption, non-readable — *Secrets live in config/env; `musubi config` prints them* — [source](https://api.edgegap.com/swagger.json)
- 🟡 **GameLift** · IAM policy examples — Player-facing backend policy, admin policy, fleet role policy examples — *key roles documented in README* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-iam-policy-examples.html)
- 🟡 **Photon** · Cheat protection by determinism — clients can only send input; checksums/desync detection & dumps; replays for verification; server-side simulation via custom plugin (Enterp… — *tick hashes for desync detection (tick_hash 2 files) + authoritative server sim free; no deterministic client sim to verify* — [source](https://doc.photonengine.com/quantum/current/manual/cheat-protection)
- 🟡 **Photon** · Checksum interval / desync dump — periodic frame checksums compared on server, dump on mismatch — *tick hashes compared server-side; no frame dump tooling* — [source](https://doc.photonengine.com/quantum/current/manual/cheat-protection)
- 🟡 **Steamworks** · Secure session tracking — `StartSecureMultiplayerSession/EndSecureMultiplayerSession`, `ReportCheatData` (blob), `RequestVacStatusForUser(session_id)` — *match sessions tracked (`match_presence`), but no opaque cheat-data blob ingestion* — [source](https://partner.steamgames.com/doc/webapi/ICheatReportingService)
- 🟡 **UGS Multiplayer** · Unity Moderation Unity package — `com.unity.services.moderation` 1.0.2 (report submission from client) — *`POST /v1/reports` from clients; no Unity package.* — [source](https://packages.unity.com/com.unity.services.moderation)

**P2 · analytics/observability** — owner: musubi-server metrics.rs/telemetry.rs/events.rs + musubi-integrations sinks

- 🟡 **Beamable** · Microservice metrics in Portal — CPU/memory utilization per service, status/deployments — *Prometheus `/metrics` incl. per-module fuel/time; no UI* — [source](https://docs.beamable.com/docs/microservice-framework)
- 🟡 **Beamable** · Telemetry API (`AnalyticsTracker.TrackEvent`) — Custom `CoreEvent` payloads; client batching default max 100 per request with size/time flush; sources Client/Platform/GameServer — *`/v1/analytics/events` batch ingest exists (README analytics row) but forwarded to external sink only* — [source](https://docs.beamable.com/docs/telemetry-overview)
- 🟡 **brainCloud** · App analytics counts via API — `SysGetDailyCounts`, `SysGetMonthlyCounts` — *Prometheus metrics + Zanshin analytics; no in-API daily/monthly user counts endpoint* — [source](https://docs.braincloudservers.com/api/capi/globalapp/sysgetdailycounts)
- 🟡 **brainCloud** · Logs: Recent Errors, Server (S2S) logs, Unreadable Requests, Job Queue, Security Logs, per-user request logs — portal log browsers with filters/export; user-level request/response log for debugging — *request recordings/replay (`/v1/admin/dev/recording`), audit log, tracing; no per-user request log UI* — [source](https://docs.braincloudservers.com/learn/portal-tutorials/user-monitoring/logs)
- 🟡 **brainCloud** · SuperRTT (system connections) — `DisconnectAllAppSessions`, `LogState` (private licensees) — *admin session revoke per account; no app-wide disconnect-all* — [source](https://docs.braincloudservers.com/api/s2s/superrtt)
- 🟡 **Edgegap** · Distributed Relay network — Hundreds of locations; telemetry-based relay choice per session; separate relay API token; status "work in progress" — *Musubi relays client-hosted matches through its own cluster nodes (WS/QUIC/WebTransport/WebRTC/UDP, ≤64 players, lz4, 4 channel reliabilities) but is not a gen…* — [source](https://docs.edgegap.com/learn/distributed-relay)
- 🟡 **EOS** · Metrics Interface: BeginPlayerSession/EndPlayerSession (account id type, display name, controller type, server IP, custom GameSessionId) → Game Analytics dashboard; automatic CCU + game-server counts + retention (D1/D7...) from SDK init — Only via SDK; no external metrics tooling allowed — *Prometheus /metrics (CCU, sessions, matches) + Zanshin analytics events integration + OTel; no built-in retention/DAU dashboards (rg -i retention → data retent…* — [source](https://dev.epicgames.com/docs/epic-online-services/player-and-game-data/eos-metrics-interface)
- 🟡 **GameLift** · CloudWatch metrics (fleet/instance) — ActiveInstances, DesiredInstances, IdleInstances, MaxInstances, MinInstances, PercentIdleInstances, InstanceInterruptions, RecycledInstance… — *Prometheus `/metrics` + OpenTelemetry for server; fleet gauges (ready/allocated); retention = operator's Prometheus* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/monitoring-cloudwatch.html)
- 🟡 **GameLift** · CloudWatch metrics (game/player sessions) — ActivatingGameSessions, ActiveGameSessions, AvailableGameSessions, PercentAvailableGameSessions, GameSessionInterruptions, CurrentPlayerSes… — *match counts, allocation latency, matchmaker waits exist; no player-session activation / abnormal termination metrics* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/monitoring-cloudwatch.html)
- 🟡 **GameLift** · Fleet debugging guide — Phase-by-phase failure causes, install-script events, remote connect during ACTIVATING — *`musubi doctor`, `GET /v1/server/fleets/{id}/servers` states* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/fleets-creating-debug.html)
- 🟡 **GameLift** · Metrics setup / scenarios / troubleshooting — Setup page, scenario playbooks (crash, lag), troubleshooting — *README ops docs; no scenario playbooks* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-servers-metrics-scenarios.html)
- 🟡 **GameLift** · Monitoring overview — Console, CloudWatch, CloudTrail, server telemetry, logs matrix — *Prometheus/OTel/audit* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/monitoring-overview.html)
- 🟡 **Hathora** · Log/metrics retention — 72 h default; Enterprise extended on request — *`[retention]` days per table for events/audit/chat; no process-log retention because logs aren't captured* — [source](https://hathora.dev/docs/how-hathora-works/telemetry)
- 🟡 **Hathora** · Process search by roomId / processId / external id, filter region/status/created-before — Console + API — *`GET /v1/matches?label=…`, admin cluster matches; no unified process search with time filters* — [source](https://hathora.dev/docs/how-hathora-works/telemetry)
- 🟡 **Hiro/Satori** · Identify (anonymous -> permanent identity merge with property/event precedence rules); server events API for identity-less or multi-identity batches — `POST /v1/server-event` with API key — *musubi account merge (0061) merges accounts/wallets/inventory; no analytics identity merge* — [source](https://heroiclabs.com/docs/satori/guides/server-events/)
- 🟡 **Hiro/Satori** · Server-Event API: name, optional UUID id (dedupe), metadata, value, timestamp, identity_id/session_id optional; partial batch acceptance — Server-side events — *musubi event bus accepts module/server-emitted events; Zanshin forwarding; no analytics store* — [source](https://heroiclabs.com/docs/satori/guides/server-events/)
- 🟡 **Hiro/Satori** · Sessions: `session_token_expiry_sec` (1.5 d default) / `session_refresh_token_expiry_sec` (7 d); client-, server- and system-session modes; session count/duration metrics; events before auth — Session management — *musubi session TTLs configurable (`[session]`), no playtime analytics* — [source](https://heroiclabs.com/docs/satori/concepts/sessions/session-management/)
- 🟡 **Photon** · Chat counters — CCU, channels, messages per region — *Prometheus chat metrics; no dashboard* — [source](https://doc.photonengine.com/chat/current/reference/counter-analytics)
- 🟡 **Photon** · Counters per app — CCU, rooms, msg/s, bandwidth, per region; premium counters on Enterprise — *Prometheus/OTel; no per-app multi-tenant counters (musubi is single-tenant)* — [source](https://doc.photonengine.com/realtime/current/reference/counter-analytics)
- 🟡 **PlayFab** · MPS/Party/Lobby/Matchmaking PlayStream events: matchmaking_match_found / ticket_completed / user_ticket_completed / user_ticket_invite, server_requested, server_state_changed, vm_assigned/vm_state_change/vm_unhealthy/vm_unassignment_started/vm_remote_user_created, build_* (alias, region status/utilization, deleted), crash_detected, game_asset_deleted, certificate_uploaded/deleted, secret_added/deleted, monitoring_output_saved, player_photon_session_authenticated — Events reference — *Fleet & match events on event bus (server allocated/ready/stopped, match created/ended, matchmaking found); no VM/certificate/secret/crash events* — [source](https://learn.microsoft.com/gaming/playfab/api-references/events/)
- 🟡 **PlayFab** · PlayStream built-in event catalog (~157 event pages): player_* (logged_in, added_title, banned, linked/unlinked_account, statistic_changed, vc_item_purchased, realmoney_purchase, receipt_validation, redeemed_coupon, ad_* 7 events, reported_as_abusive, display_name_filtered, data_exported, device_info…), character_*, entity_* (created, logged_in, objects_set, files_set, language_updated, executed_cloud_script, rank_on_leaderboard_version_ended), group_* (12), items_* economy v2 (added/deleted/purchased/redeemed/subtracted/transferred/updated, item_moderation_state_changed, item_reported, review_*), matchmaking_* (4), MPS (build_*, server_requested/state_changed, vm_* 7, crash_detected, secret_added…), title_* (api_settings_changed, catalog_updated, client_rate_limited_alert, tasks…), studio_* , sent_email / sent_push_notification, auth_token_validated, client_focus_change/session_start — Documented schema per event + 94 data types — *README event bus: typed server events for account/auth/economy/match/fleet/moderation (migration 0027 events, `events.rs`) forwarded to webhooks; fewer event k…* — [source](https://learn.microsoft.com/gaming/playfab/api-references/events/)
- 🟡 **PlayFab** · Title/studio events: title_api_settings_changed, title_added_cloudscript, title_created/aborted/completed_task, title_client_rate_limited_alert, title_deleted_master_player, title_initiated_player_password_reset, studio_created/tier_updated/user_added/invited/removed, tenancy_connector_onboard, enabled_for_title — Events reference — *Audit log captures admin config changes; no studio/tenant events, no rate-limit alert event* — [source](https://learn.microsoft.com/gaming/playfab/api-references/events/)
- 🟡 **Pragma** · Logging — logback; per-logger levels hot-reload; file logger; operator RPC to fetch N log lines; SDK verbose flag. — *`tracing` JSON logs with `RUST_LOG`; no runtime level change or remote log fetch.* — [source](https://pragma.gg/docs/2026.3.0/concepts/monitoring/logging)
- 🟡 **Rivet** · Debugging guide: list actors by name/key/id, read KV, inspect runner connectivity, pool errors — docs — *`GET /v1/server/matches`, `/worlds`, `/gameservers`, `/cluster/nodes`; no written runbook* — [source](https://rivet.dev/docs/debugging/)
- 🟡 **Rivet** · Live latency shown per runner in dashboard list — Oct 2025 — *node heartbeats in `GET /v1/server/cluster/nodes`; no latency UI* — [source](https://rivet.dev/changelog/2025-10-20-weekly-updates)
- 🟡 **SpacetimeDB** · Dashboard metrics — Per-DB Overview (identity, status, table/reducer counts, energy), Usage breakdown (bytes scanned/written, index seeks, CPU instr, bandwidth… — *`GET /metrics` Prometheus (27 files), pprof/flamegraph (`/v1/admin/debug/profile`); no built-in UI (console not written)*
- 🟡 **SpacetimeDB** · Full row history retention — Docs claim the commitlog retains full history of changed rows (compression guidance) — *musubi has audit log (`0073 audit`) and match/world journals; no general row-history for all tables*
- 🟡 **UGS LiveOps** · Consent management (opt-in/opt-out, `EndUserConsent` API, PIPL/GDPR regions) — SDK 6.1+ flow; legacy flow deprecated — *Age gate / parental consent (`/v1/account/parental/request`, COPPA) and GDPR export/delete; no analytics consent flag gating event forwarding* — [source](https://docs.unity.com/en-us/analytics/compliance/opt-in-compliance)
- 🟡 **UGS LiveOps** · Environment support, user roles for analytics — Roles: viewer/manager — *Server-key roles (support/liveops/ops); no analytics-specific roles* — [source](https://docs.unity.com/en-us/analytics/get-started/user-roles)
- 🟡 **UGS LiveOps** · Event Browser (debug) — Last 100 events / 48 h, up to 10 min delay, view-only — *`GET /v1/admin/audit` + webhook delivery logs; no event tail UI* — [source](https://docs.unity.com/en-us/analytics/events/event-browser)
- 🟡 **UGS LiveOps** · REST Collect API — `POST /api/analytics/collect/v1/projects/{P}/environments/{env}`; ≤5 MB body; events >31 days old or >24 h in future rejected; PIPL consent… — *Server-side event ingestion only (`/v1/server/events`, server key); no client-authenticated collect endpoint* — [source](https://docs.unity.com/en-us/oas-analytics/1.0.0)
- 🟡 **UGS LiveOps** · Standard events auto-collected by SDK (gameStarted, gameRunning every 60 s, transactions, ad impressions, acquisitionSource) — Batched upload every 60 s (not configurable, manual `Flush`) — *Server emits `musubi.events` bus (`0027_events.sql`) for backend facts (login, purchase, match…) and forwards account-bound events to Zanshin; no client teleme…* — [source](https://docs.unity.com/en-us/analytics/events/record-event)

**P2 · runtime/extensibility** — owner: musubi-runtime + musubi-idl (WASM host ops, `/v1/rpc/*`, hooks, `/v1/admin/webhooks`)

- 🟡 **AccelByte** · CSM API (107): app lifecycle V1–V5 (create, deploy, start/stop, status progress), images, configurations & env vars, deployments, App UI (4), async messaging topics (5), managed resources NoSQL/SQL/KV (35), notification subscriptions (deploy/crash alerts to admins), resource limits, extend files — *modules deploy/canary/promote/rollback + status via CLI/admin API; no managed DB provisioning, no crash alert subscriptions*
- 🟡 **Beamable** · Deployment plans (`deploy plan` / `deploy release`) — Builds x64 Docker images, `timestamp.plan.json` in `.beamable/temp/plans`, replace (default) vs merge plans, optional `--health` boot valid… — *`musubi deploy` manifest + module publish/canary/promote/rollback; no diff/plan preview step, no container builds (wasm only)* — [source](https://docs.beamable.com/docs/cli-ms-deployment)
- 🟡 **Beamable** · Enable/disable deployed services — `project enable/disable`, disabled services cost nothing; temporarily disable via Portal — *module unpublish/drop; no per-environment enabled flag* — [source](https://docs.beamable.com/docs/cli-ms-deployment)
- 🟡 **Beamable** · Events calendar & phases API — `events/calendar`, `/running`, `/applycontent`, `/endphase`, `/refresh`, `event-players/{id}/score/claim` — *Live events + tournament windows; no phased event object* — [source](https://docs.beamable.com/reference/events-overview)
- 🟡 **Beamable** · Group users: recommended groups, search, petition/apply, invite, kick, donations — `group-users/{id}/recommended`, `/search`, `groups/{id}/petition`, `/apply`, `/donations` — *Search/apply/invite/kick exist; no recommended-groups endpoint or donation ledger* — [source](https://docs.beamable.com/reference/groups-overview)
- 🟡 **Beamable** · Inventory transfer / preview / multipliers — `PUT /object/inventory/{id}/transfer` (player-to-player), `/preview` (dry-run update), `/multipliers` — *P2P trades + server tx exist; no dry-run preview endpoint, no multipliers (reward modifiers approximate)* — [source](https://docs.beamable.com/reference/inventory-overview)
- 🟡 **Beamable** · Legacy systems still exposed — Entitlements (deprecated, use Inventory), Promos/promo codes, PvP defs, Shards (game shards accepting/preferred), Timers, Cloud campaigns/t… — *Coupons/entitlements/timers exist; no shard-assignment legacy API (worlds/cells differ)* — [source](https://docs.beamable.com/reference)
- 🟡 **Beamable** · Realms API — customer/game/project CRUD, `project/promote` (content promotion), `project/rename`, `launch-message`, `plans`, `promotion`, `customer-alia… — *Projects/environments + config; no promotion/launch-message/plans* — [source](https://docs.beamable.com/reference/realm-administration-overview)
- 🟡 **Beamable** · Stats search & subscribe — `POST /basic/stats/search` (query players by stat criteria), `PUT /basic/stats/subscribe`, batch get — *Stats query exists per account; cross-player search by stat criteria only via audiences/leaderboards* — [source](https://docs.beamable.com/reference/stats-overview)
- 🟡 **brainCloud** · API Blocking — portal screen blocking client-callable APIs; RestrictClientCalls script — *per-key role scoping and rate limits (`limits.rs`); no per-operation client blocklist* — [source](https://updates.braincloudservers.com/release-4.13.1-is-live-2IvXzi)
- 🟡 **brainCloud** · Batch user scripts — `RunBatchUserScript[AndCompletionScript]`, `...ForProfiles`, `...ForSegments`, `ScheduleBatchUserScript`, `CancelUserScriptBatch`; big-mess… — *audience-targeted mail/push; no “run script per user over a segment with completion callback” primitive* — [source](https://docs.braincloudservers.com/api/s2s/script/runbatchuserscript)
- 🟡 **Colyseus** · Message handling: declarative `messages = {type: handler}` map, `"*"` fallback, runtime `this.onMessage()` returning unbind, `onMessageBytes` raw `Uint8Array`, type string or number — *`match_input` / `match_data_send` are opaque payloads routed to one handler (`match_<module>_tick/…`); module dispatches by its own convention* — [source](https://docs.colyseus.io/room)
- 🟡 **Edgegap** · Deprecated endpoints documented — v1 Deploy Server, update deployment properties — *No deprecation policy documented* — [source](https://docs.edgegap.com/docs/api/deprecated)
- 🟡 **Edgegap** · PlayFab bridge — CloudScript function + automation rule from PlayFab matchmaker queue — *PlayFab data importer only* — [source](https://docs.edgegap.com/docs/tools-and-integrations/playfab-bridge)
- 🟡 **EOS** · Ecom Web API v3/v4: ownership check, entitlements list/redeem, ownership & entitlement verification tokens (RS512 JWT, 5 min) + public keys endpoint, clawbacks, CTC token reporting — Backend-to-backend — *musubi consumes Epic entitlements via s2s (/v1/economy/entitlements/sync, Epic provider) but does not issue ownership tokens for third parties* — [source](https://dev.epicgames.com/docs/web-api-ref/ecom-web-apis)
- 🟡 **EOS** · Sanctions Web API: active sanctions per PUID, /sanctions/v1/sync event feed, deployment-wide list/create/patch/delete, pending approve, appeals create/list, metadata ≤25 pairs (key ≤64, value ≤128), paging limit — Trusted client token — *admin ban endpoints + audit log + event bus; no sync feed/appeals* — [source](https://dev.epicgames.com/docs/web-api-ref/sanctions-web-apis)
- 🟡 **EOS** · Voice Web API: POST /rtc/v1/{deployment}/room/{room} create room tokens (per PUID) returning media server endpoint; DELETE participant (kick); POST participant (mute); Voice:createRoomToken/kick/mute policy actions — Trusted server — */v1/voice/token mints Aurix tokens; no media server, kick/mute passthrough only if provider supports* — [source](https://dev.epicgames.com/docs/web-api-ref/voice-web-api)
- 🟡 **GameLift** · Encryption at rest — Builds/scripts in S3 with SSE; service data encrypted with AWS-managed keys — *Postgres at rest is operator's responsibility; secrets encrypted in `musubi-storage`? not verified → partial* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/data-protection.html)
- 🟡 **GameLift** · Fleet aliases — `CreateAlias{RoutingStrategy SIMPLE(fleetId) / TERMINAL(message)}`, `ResolveAlias`, blue/green switch of fleet behind alias, console alias… — *pools decouple clients from fleets; builds promote within a fleet; no alias object / terminal message routing* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/aliases-intro.html)
- 🟡 **GameLift** · Realtime Servers scripts — `CreateScript` (zip ≤5 MB or S3), Node.js script with callbacks (onMessage, onPlayerConnect, …), `UpdateScript`; Realtime fleets on EC2 onl… — *musubi `musubi-runtime` WASM match modules (authoritative actors, JS/TS via `musubi module`) – more capable but not Node.js scripts* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_CreateScript.html)
- 🟡 **GameLift** · Realtime client SDK — C# client SDK 1.2.0 (TCP reliable + UDP unreliable, Unity); FlexMatch backfill not supported with Realtime — *musubi has WS/QUIC/WebTransport/WebRTC/UDP transports but no shipped client SDK (Unreal scaffold only)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/reference-serversdk.html)
- 🟡 **Nakama** · Dev tooling: Jest testing guide for TS modules, Delve debugging via `nakama-dsym` image, Go dependency pinning guide, Nakama project template, `nakama check` module validation — OSS — *`musubi dev` local loop, fixtures, request record/replay, bench/soak (README dev rows); no language-specific test harness docs* — [source](https://heroiclabs.com/docs/nakama/guides/server-framework/typescript-testing/)
- 🟡 **Nakama** · `RegisterMatchmakerMatched` (create authoritative match for matched users or return match id), `RegisterMatchmakerOverride`, `RegisterMatchmakerProcessor` — OSS — *`hook_matchmaker_matched` creates match / allocates fleet server; no override/processor* — [source](https://heroiclabs.com/docs/nakama/server-framework/go-runtime/function-reference/)
- 🟡 **Pragma** · Broadcast notifications — Push to every player / partner / operator session; operator RPC `NotifyAllActiveSessionsOperatorV1` (no code); batching `notifyAllActiveSes… — *notifications to accounts/audiences/all (`POST /v1/server/notifications` with audience `all`) and realtime `Notification`; no partner/operator-session broadcas…* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/broadcast-notifications)
- 🟡 **Pragma** · Custom errors (application vs service) — Proto application errors with typed SDK accessors (`ReadError<T>`), `applicationRequire*` helpers; service errors enum. — *module RPCs return `{code, message, details}` JSON errors (RFC-7807 style); not typed in SDK.* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/custom-errors)
- 🟡 **Pragma** · Custom persistence (DAO node services) — `PartitionedDaoNodeService`/`UnpartitionedDaoNodeService`, Liquibase changelogs, own tables. — *modules use storage collections (JSON, indexes, contracts) — no raw SQL tables per module.* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/persistence)
- 🟡 **Pragma** · Custom services (`DistributedService`, `@PragmaService`, `@PragmaRPC`) — Proto RPC pairs, session-type gating, routing methods, conformance tests, SDK regeneration. — *WASM module RPCs (`POST /v1/rpc/{id}`, socket `rpc`) with IDL; no generated typed client stubs into Unity/Unreal for custom RPCs beyond IDL codegen for Unreal…* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/custom-service)
- 🟡 **Pragma** · Plugins (`@PragmaPlugin`, `ConfigurablePlugin`) — Swap engine behaviour at ~30 plugin points (Account, Party, Matchmaking, GameInstance, GameServerProvider, Fleet, OrderProvider, Telemetry,… — *WASM hooks on ~40 events + webhooks; cannot replace core algorithms (e.g. matchmaker loop) wholesale.* — [source](https://pragma.gg/docs/2026.3.0/concepts/customization/plugins)
- 🟡 **Rivet** · Durable timers `ctx.sleep` / `sleepUntil`, `ctx.loop` durable loops, joins, races, fan-out — docs — *`sleep_secs` and `wait_signal` parking (README L418, `workflows.rs`); no join/race/fan-out primitives, single linear step chain* — [source](https://rivet.dev/workflows/docs/timers-and-concurrency/)
- 🟡 **Rivet** · Reschedule actor to another runner (`POST /actors/{id}/reschedule`), force sleep (`POST /actors/{id}/sleep`) — ops controls — *`POST /v1/server/matches/{id}/migrate` / mesh handover; no force-sleep* — [source](https://rivet.dev/docs/debugging/)
- 🟡 **Rivet** · Schedule history (`maxHistory` 100) and `c.cron.list/delete` — docs — *server cron jobs are config-defined (`schedules.rs`), no runtime list/history API* — [source](https://rivet.dev/actors/docs/schedule/)
- 🟡 **Rivet** · `c.schedule.after(ms, action, args)` / `schedule.at` durable one-shot alarms that survive sleep/restart/upgrade — per-actor — *server-side `musubi-server/src/schedules.rs` + workflows `delay_secs`/`sleep_secs`; no per-actor alarm API callable from module code (`rg -i alarm` = 0)* — [source](https://rivet.dev/actors/docs/schedule/)
- 🟡 **Rivet** · `onStateChange` hook — fires on every state mutation — *`storage_subscribe` → `storage_update` notifies clients of storage changes; no in-actor hook* — [source](https://rivet.dev/actors/docs/actor-configuration/)
- 🟡 **SpacetimeDB** · Automatic (compatible) migrations — Add tables/indexes/trailing default columns, remove some constraints, visibility changes OK; unsupported: drop non-empty table, remove/modi… — *`musubi migrate` runs fixed SQL migrations for the backend's own schema; module state schema is IDL-versioned; no automatic diff of developer-defined tables*
- 🟡 **SpacetimeDB** · Type system (SATS/BSATN) — Algebraic types, 8–256-bit ints, sum/product types, BSATN binary + SATS-JSON encodings; schema endpoint `GET /v1/database/{db}/schema?versi… — *IDL + JSON/binary realtime encoding; `GET /openapi.json` for REST; no equivalent self-describing module schema with client codegen*
- 🟡 **Steamworks** · Response formats json/xml/vdf — `format=` param; 64-bit ints as strings in JSON — *JSON only (plus postcard binary on the socket); no XML/VDF — irrelevant for most studios* — [source](https://partner.steamgames.com/doc/webapi_overview/responses)
- 🟡 **Steamworks** · Versioned methods (v1/v2…) kept backward compatible — old versions "continue to be usable" (ISteamNews, GetPublisherAppOwnership v3) — *single `/v1` namespace; no documented multi-version policy yet (alpha)* — [source](https://partner.steamgames.com/doc/webapi_overview)
- 🟡 **UGS LiveOps** · Logging + log query (Observability API, filter language) — OpenTelemetry logs; `GET /P/E/logs` 10 rps / 2000 per hour; dashboard log viewer — *Module `log` host op → structured server logs + OpenTelemetry traces (`otel` in 3 crates), Prometheus `/metrics`; no hosted log-query API/UI — rely on your log…* — [source](https://docs.unity.com/en-us/cloud-code/logging/overview)
- 🟡 **UGS LiveOps** · Module metadata tags — ≤10 tags, key/value ≤128 chars — *Module versions carry name/version/notes; no free-form tags* — [source](https://docs.unity.com/en-us/cloud-code/modules/how-to-guides/module-structure)
- 🟡 **UGS LiveOps** · Scheduled start/end of an override — Date-time window; enable/disable toggle — *Live events (`/v1/liveops/live-events` via Zanshin) and store listing availability windows; no scheduled config variant in core* — [source](https://docs.unity.com/en-us/remote-config/game-overrides-and-settings)
- 🟡 **UGS LiveOps** · Script versioning, publish vs draft, test-run previous versions — Dashboard editor, test run with generated test player id, version history — *Module versions + rollback exist; no "draft/unpublished" state or dashboard test runner (CLI `musubi dev` hot reload + request record/replay instead)* — [source](https://docs.unity.com/en-us/cloud-code/scripts/how-to-guides/test-run)
- 🟡 **UGS LiveOps** · Unit-test project templates (Rider/VS) — Template with excluded test project — *`musubi-sim-sdk` + fixtures/replay for Rust modules; no scaffolding command for module test projects* — [source](https://docs.unity.com/en-us/cloud-code/modules/how-to-guides/unit-testing)

**P2 · SDKs** — owner: `sdk/` (generate from `GET /openapi.json` + IDL codegen; shared `musubi-client-core`)

- 🟡 **Agones/OM** · Beta counters: `GetCounterCount`, `SetCounterCount`, `IncrementCounter`, `DecrementCounter`, `SetCounterCapacity`, `GetCounterCapacity` (errors at capacity/zero) — Batched 1 s, SDK-local read-your-writes — *Only `POST /v1/gameserver/players {players, capacity}`*
- 🟡 **Agones/OM** · Local SDK server binary (`sdk-server --local`, `--file gameserver.yaml` mock, `--feature-gates`, `--sdk-name`) for macOS/Linux/Windows — Released per version — *`musubi dev` runs the whole backend locally (README:operating commands); no standalone mock of the game-server API*
- 🟡 **Agones/OM** · SDK conformance tests per language (`test/sdk/*`) run in CI — Repo — *CI runs the server suite against Postgres (README:status); no SDK conformance matrix* — [source](https://github.com/agones-dev/agones/tree/main/test/sdk)
- 🟡 **Agones/OM** · `SetLabel(k,v)` / `SetAnnotation(k,v)` with forced `agones.dev/sdk-` prefix (isolation / attack surface) — k8s label syntax limits — *`POST /v1/gameserver/label` replaces the whole labels JSON; no annotation channel, no prefix isolation*
- 🟡 **Agones/OM** · `WatchGameServer(cb)` — server-streaming callback on every change (state, labels, annotations, allocation metadata) — Push — *`GET /v1/gameserver/watch?since=<version>&timeout_secs` long-poll on `version` counter (mig 0067 trigger) — pull-based, one change per round-trip*
- 🟡 **Colyseus** · Client join methods `joinOrCreate`, `create`, `join`, `joinById` (private rooms joinable by id → invite links), `consumeSeatReservation` (reservation from backend/queue/lobby, `publicAddress` honoured, 15 s expiry) — all SDKs — *HTTP/WS API: `POST /v1/matches` (create), join by id, invite codes, lobbies, matchmaker tickets — but no SDK; studios hand-roll clients* — [source](https://docs.colyseus.io/sdk)
- 🟡 **Colyseus** · Latency & multi-region: `client.ping({pingCount})` before join, `room.ping()` during, `Client.selectBestEndpoint(endpoints, latencyOptions)` picks lowest-latency region; `room.clock.serverNow()/rtt()` — SDK — *`GET /v1/regions` + ping endpoints per region and server-side region scoring in matchmaker (`ping windows`); no client helper* — [source](https://docs.colyseus.io/sdk)
- 🟡 **Colyseus** · Messaging: `room.send(type, payload)`, `sendBytes(Uint8Array)`, `onMessage`, request/response `room.request(type, payload, {mode: "unreliable"})` with timeout, `ctx.reject` business errors — all SDKs — *`MatchData`/`Rpc` realtime messages with request ids; no SDK helpers* — [source](https://docs.colyseus.io/sdk)
- 🟡 **GameLift** · ActivateGameSession / GetGameSessionId / GetTerminationTime — Server confirms activation; termination time for Spot interruption — *allocation implies activation; no termination-time API (no Spot)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk5-cpp-actions.html)
- 🟡 **GameLift** · Match backfill from server — `StartMatchBackfill` / `StopMatchBackfill`; `onUpdateGameSession(updateReason)` — *`backfill` flag on allocation & queue `backfill_ttl`; no per-session backfill ticket API from the server* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk5-cpp-actions.html)
- 🟡 **GameLift** · SDK data types — LogParameters, MetricsParameters, ProcessParameters, UpdateGameSession, GameSession, ServerParameters, Player, PlayerSession, AttributeValu… — *JSON bodies documented in README/OpenAPI (`musubi idl`)* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/integration-server-sdk5-cpp-datatypes.html)
- 🟡 **GameLift** · SDK reference index (client/server/plugins/downloads) — Version table of all SDKs and plugins — *`musubi idl` codegen* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/reference-intro.html)
- 🟡 **Hathora** · CreateFleet / UpdateFleet — `name`, `nodeShapeId`, `autoscalerConfig{scaleUpThreshold…}`, per-region `nodeBaseline` (SDK 2.25) — *`POST /v1/server/fleets` with ScalePolicy; no node shapes* — [source](https://www.npmjs.com/package/@hathora/cloud-sdk-typescript (v2.25.6 models))
- 🟡 **Hathora** · CreateRoom / GetRoomInfo / GetActiveRoomsForProcess / GetInactiveRoomsForProcess / DestroyRoom / SuspendRoom / GetConnectionInfo / UpdateRoomConfig / ResumeRoom — see Rooms rows; `RoomAllocation` history {processId, scheduledAt, unscheduledAt} — *matches + gameservers endpoints* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · DeploymentV1/V2 CPU & memory ranges — V1 `planName` Tiny/Small/Medium/Large; V2 `requestedCPU` 0.5–4 (one decimal), `requestedMemoryMB` 1024–8192 — *`vcpu` for accounting only, no enforcement* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · GetBuilds / GetBuild — Org-wide list; fields buildId, buildTag, status, imageSize, contentHash, regionalContainerTags[], createdAt/By, startedAt, finishedAt, dele… — *`GET …/fleets/{id}/builds` per fleet (version, state, canary_percent/regions, notes); no image metadata* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · GetDeployments / GetLatestDeployment / GetDeployment / CreateDeployment — Full `DeploymentV3` object incl. `createdBy`, `env[{name,value}]`, `additionalContainerPorts[{name,port,transportType}]` — *fleet spec/build endpoints; see Deployments rows* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · GetFleets / GetFleetRegion / UpdateFleetRegion / GetFleetMetrics — One fleet per org (V1); region config `cloudMinVcpus`, `scaleIncrementVcpus`; metrics series — *`GET/POST /v1/server/fleets`, `POST …/scale`, `GET …/usage`, `GET …/demand`* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · GetLatestProcesses / GetProcessesCountExperimental / CreateProcess / GetProcess / StopProcess / GetProcessMetrics — see Processes rows — *fleet servers endpoints* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · GetMetrics (legacy) — Per-process metrics, superseded by ProcessesV3 metrics — *see Processes metrics* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · GetNodes — Node inventory: nodeId, host, hosting, cpu, memory, gpu, region, status, onlineSince — *`/v1/admin/cluster/*` for musubi nodes only* — [source](https://www.npmjs.com/package/@hathora/cloud-sdk-typescript (v2.25.6 models))
- 🟡 **Hathora** · Per-process metrics — `GetProcessMetrics`: cpu, memory, rate_egress, total_egress, active_connections (+ gpu_memory, gpu_utilization in SDK); start/end/step — *`/metrics` Prometheus covers musubi nodes, matches, transports; fleet usage is vCPU-hours per build (R194); no per-game-server CPU/mem/egress time-series (`rg…* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · Scoped org tokens — Scopes per resource `*:read` / `*:read-write` for billing, builds, deployments, applications, processes, fleets, orgs, tokens (+ tickets, l… — *roles are coarse bundles (5 roles), not per-resource read vs read-write scopes* — [source](https://hathora.dev/docs/guides/scopes)
- 🟡 **Hathora** · `ExposedPort{host, port, name, transportType}` — Named ports; default port supports TLS; up to 3 ports — *address/port only; no named ports/transport attribute* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Hathora** · `RoomAllocation` history — Every (re)schedule of a room onto a process with `scheduledAt`/`unscheduledAt` — *match history tables (migrations history) for in-process matches; dedicated-server allocation history only as `ready_at`/`terminated_at`* — [source](https://hathora.dev/api (swagger.json, archived))
- 🟡 **Metaplay** · Client build version (`BuildVersion{Version, BuildNumber, CommitId}`) attached to incident reports and dev commit check — *client version header checked against min version; not surfaced in reports* — [source](https://docs.metaplay.io/feature-cookbooks/unity-sdk-features/client-build-version)
- 🟡 **Nakama** · Protobuf or JSON socket encoding (`format=protobuf`), `lang` param, `status=true/false` on connect (appear online) — OSS — *JSON + binary framing over WS/QUIC with LZ4; protobuf wire format not offered (musubi-proto is Rust serde)* — [source](https://heroiclabs.com/docs/nakama/concepts/sockets/)
- 🟡 **Photon** · Bot SDK — HFSM, behaviour trees, utility theory, blackboard, visual editor; bots fill rooms/replace disconnected players; 3.10.6 Oct 2026 — *matchmaker backfill + "bot" mentions (35 files, mostly chat/bots filler?) — no AI authoring toolkit* — [source](https://doc.photonengine.com/quantum/current/addons/bot-sdk/overview)
- 🟡 **Pragma** · Client cache & entity lookups — SDK cache auto-synced; `GetEntityByInstanceId`, `GetEntitiesByName`, `GetUniqueEntityByName`, `AllInstanceIds`. — *storage subscriptions push changes over WS (`StorageSubscribe`), but the Unreal SDK offers raw objects, no typed entity cache helpers.* — [source](https://pragma.gg/docs/2026.3.0/concepts/player-data/player-data-reference)
- 🟡 **Pragma** · Client-version enforcement events — `OnGameClientVersionUpdateRequired`, `OnPartyClientVersionMismatch` — backend tells clients when their build is incompatible with servers. — *`Status`/`Error` with min-version code exists on connect; no party-level mismatch event.* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/multiplayer-reference/game-server-tasks/connect-players)
- 🟡 **Pragma** · Friend / block events — `OnFriendInviteReceived/Accepted/Declined/Canceled`, `OnFriendListChanged`, `OnFriendUpdated`, `OnRemovedAsFriend`, `OnSentInvitesChanged`,… — *`Notification` kinds (`friend.request`, `friend.accepted`, `friend.removed`) + presence; no sent/received-list-changed or blocked-list-changed events.* — [source](https://pragma.gg/docs/2026.3.0/concepts/social/friends/friends-tasks)
- 🟡 **Pragma** · Initialize multiplayer services (SDK) — `Party`, `Matchmaking`, `GameInstance` services, `MultiplayerApi` facade with `OnGameInstanceReady` etc. — *Unreal plugin exposes raw WS messages + helpers, not a facade with full event set.* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/multiplayer-reference/initialize-multiplayer-services)
- 🟡 **Pragma** · Party events — `OnJoinedParty`, `OnLeftParty`, `OnRemovedFromParty`, `OnPlayerJoinedParty`, `OnPlayerLeftParty`, `OnPartyPlayersChanged`, `OnPartyUpdated`… — *realtime `PartyState` full-state push covers the same changes, but the Unreal plugin exposes one generic party-state delegate, not per-change typed events.* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/multiplayer-reference/party-tasks/create-parties)
- 🟡 **Pragma** · Party invite events — `OnPartyInviteReceived`, `OnPartyInviteAccepted`, `OnPartyInviteDeclined`, `OnPartyInviteCancelled`, `OnPartyInviteRevoked`, `OnReceivedPar… — *`PartyInvite` notification + accept/decline messages; no revoke/cancel notifications to the invitee (`realtime.rs`).* — [source](https://pragma.gg/docs/2026.3.0/concepts/multiplayer/multiplayer-reference/party-tasks/invite-tasks)
- 🟡 **Pragma** · SDK configuration — `Pragma.ini`/`DefaultGame.ini`; CLI flags `-PragmaBackendAddress`, `-PragmaPartnerBackendAddress`, `-PragmaDebug`, `-OverrideSocial/GameHos… — *Unreal plugin settings (`UMusubiSettings`: base URL, ws URL, server key) — fewer runtime overrides.* — [source](https://pragma.gg/docs/2026.3.0/concepts/configuration/sdk-configuration)
- 🟡 **Pragma** · SDK debug logging — `-PragmaDebug`, `bLogPragmaDetailedMessages`. — *`LogMusubi` category; no detailed-messages toggle documented.* — [source](https://pragma.gg/docs/2026.3.0/concepts/configuration/sdk-configuration)
- 🟡 **SpacetimeDB** · Connection lifecycle callbacks — `onConnect` (identity, token), `onConnectError` (`NextReconnect`), `onDisconnect`, `onAutomaticReconnect` — *`Session`, `Error` server messages; client side absent*
- 🟡 **SpacetimeDB** · Procedure/view calls from client — Procedures return values asynchronously; views subscribed like tables — *`Rpc` returns payload; no views*
- 🟡 **UGS LiveOps** · Cloud Save Files (large binary save files, any format) — Up to 1 GiB per file, 200 files per player, 255-char names, signed upload/download URLs, metadata, `WriteLock`, SDK `RequestTimeout` (defau… — *Blob storage exists only for chat attachments (10 MiB) and UGC (`[ugc.s3]`, ≤16 MiB, `/v1/ugc/*`); no per-player private save-file API with GiB-scale presigned…* — [source](https://docs.unity.com/en-us/cloud-save/concepts/files)
- 🟡 **UGS Multiplayer** · Client-side latency/packet-loss measurement SDK — UDP probes port 7778; WebTransport on WebGL (QoS 1.5.0); results fed to Relay region pick and matchmaker tickets — *UDP echo probe protocol exists; measuring is left to the client (no C#/C++ QoS SDK), packet loss not modelled.* — [source](https://docs.unity.com/en-us/mps-sdk/networking/qos)

**P2 · deployment/ops** — owner: musubi-cli + `deploy/` (Helm, Terraform), musubi-cluster

- 🟡 **Beamable** · Developer user manager — create-user / create-user-batch / save-user / copy-state / remove-user / update-info / ps — test accounts & state capture — *Dev fixtures (`musubi dev` seeds) ; no test-user state copy* — [source](https://docs.beamable.com/docs/cli-developer-user-manager)
- 🟡 **Beamable** · OpenAPI tooling (`oapi download/generate`, `project generate-client-oapi`, `project generate web-client`) — Download platform OpenAPI docs; generate Unity/Unreal/web clients — *OpenAPI served; `musubi idl` codegen for configs only* — [source](https://docs.beamable.com/docs/cli-oapi)
- 🟡 **Beamable** · Project lifecycle commands — `project new service/storage/common`, `project list/run (-w hot reload)/ps/stop/logs/open-swagger`, `fed list` + enable/disable federations… — *`musubi dev`, `module publish/canary/promote/rollback/versions`; no process mgmt (ps/stop/logs) for local modules* — [source](https://docs.beamable.com/docs/cli-ms-workflow)
- 🟡 **Beamable** · Telemetry collector mgmt (`beam telemetry collector start/ps/stop`, `telemetry logs/push/prune/config`) — Local OTel collector lifecycle; `BEAM_LOCAL_OTEL` — *OTel exporter config only* — [source](https://docs.beamable.com/docs/cli-telemetry)
- 🟡 **Beamable** · Token tooling (`token inspect`, `token from-refresh`), `config secret` (realm secret) — Inspect access tokens, exchange refresh — *Key management via admin API; no token inspect CLI* — [source](https://docs.beamable.com/docs/cli-token-inspect)
- 🟡 **Beamable** · `beam deploy plan/release/status/get/list/registry` — Deployment management — *`musubi deploy` + module versions; no status/list of past deployments* — [source](https://docs.beamable.com/docs/cli-deployment)
- 🟡 **Beamable** · `beam generate-interface`, `project generate-env` (`--include-secret`, `.env` output), `generate-properties`, `generate-ignore-file` — Scaffolding helpers — *`musubi idl`/`schema`; no env scaffolding* — [source](https://docs.beamable.com/docs/cli-project)
- 🟡 **brainCloud** · Compatibility flags per app — Advanced Settings: dozens of flags added with each release (default on for existing apps), searchable, release-tagged — *versioned OpenAPI/IDL and migrations; no per-app behaviour-compat switch board* — [source](https://updates.braincloudservers.com/braincloud-5.4-is-live-1aIChG)
- 🟡 **brainCloud** · Documentation per method with 10 language samples + llms.txt — every API page has C#/C++/ObjC/Java/JS/Dart/Roblox/GDScript/CloudCode/Raw samples; `llms.txt` service files for AI — *README route table + OpenAPI; no per-language samples* — [source](https://docs.braincloudservers.com/llms.txt)
- 🟡 **brainCloud** · Multi-app teams, Super users, team usage — Teams own apps; Team › Manage › Members with permissions, Builder API access, read-only Super users, Team Usage metrics (licensees) — *`[[projects]]` + operator roles (server/admin/support/liveops/ops) on keys; no team/member management UI* — [source](https://docs.braincloudservers.com/learn/portal-tutorials/managing-apps)
- 🟡 **EOS** · Block-list enforcement across EAS block list and platform (console) block lists in lobbies/sessions/P2P/RTC — Automatic in SDK — *musubi blocks (/v1/friends/block) enforced in chat/DM/invites/matchmaker avoid list; no console block-list import* — [source](https://dev.epicgames.com/docs/epic-online-services/eos-fundamentals/eos-platform-interface)
- 🟡 **GameLift** · Game server wrapper — Go wrapper runs unmodified executables on all fleet types with telemetry — *any `command` runs, but readiness/health calls are required* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-wrapper-tutorial.html)
- 🟡 **Hathora** · Max process lifetime / draining — Processes stop accepting new rooms after 6 h (2023) → killed after 24 h (Nov 2024); status `draining` — *node drain (`POST /v1/admin/cluster/nodes/{id}/drain`, SIGTERM) exists for musubi nodes; no max-age policy for fleet game servers* — [source](https://blog.hathora.dev/changelogs-august-2023/)
- 🟡 **Hathora** · `hathora build create` / `hathora deployment create -b` — Separate build vs deployment steps for pipelines — *builds via `POST …/fleets/{id}/builds` and `musubi fleet push`; no image build step* — [source](https://hathora.dev/docs/guides/ci-cd)
- 🟡 **Hathora** · `hathora deploy` one-shot build+deployment with `--from-latest` — Build tag, rooms-per-process, container port, transport, memory, cpu, env, idle timeout — *`musubi fleet push fleet.toml` pushes spec; image build/push left to CI* — [source](https://hathora.dev/docs/guides/ci-cd)
- 🟡 **LootLocker** · Player identifiers: ULID (primary), 12-char Public UID for display/sharing, legacy numeric id, guest player_identifier, platform identifiers, name — *UUID account ids + display_name; no short shareable public UID* — [source](https://docs.lootlocker.com/the-basics/core-concepts/glossary)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Debug cloud workers — `spatial project deployment worker shell`, TCP port forwarding into worker container — *fleet provider shells (ssh/k8s) not wrapped by CLI* — [source](https://documentation.improbable.io/sdks-and-data/docs/debug-cloud-worker-instances)
- 🟡 **Meshing vendors (Improbable SpatialOS)** · Metrics reference — documented worker/entity/command/egress metrics with PromQL examples — *metric names in README, no reference page* — [source](https://documentation.improbable.io/spatialos-tools/docs/metrics-reference)
- 🟡 **Meshing vendors (coherence)** · Voice — Dissonance Voice (3rd-party asset) integration, proximity voice via CoherencePlayer — *voice delegated to Aurix (token mint `/v1/voice/token`); no in-engine proximity integration* — [source](https://docs.coherence.io/manual/networking-voice)
- 🟡 **Metaplay** · Deployment troubleshooting guide, `metaplay debug logs/shell/server-status/collect-cpu-profile/collect-heap-dump/admin-request` — *`musubi doctor`, `/v1/admin/dev/profile` CPU profiles (README dev tooling row); no remote shell/heap dump CLI* — [source](https://docs.metaplay.io/cloud-deployments/troubleshooting)
- 🟡 **Metaplay** · Logic Versions: `supportedLogicVersions` range baked at build, Active Logic Version Range editable at runtime in dashboard (Client Compatibility Settings), `System.AutoUpgradeLogicVersion`, `[AddedInVersion]/[RemovedInVersion]` members, players cannot downgrade, logic-version-aware generated forms — R26 preview, GA later — *musubi: `min_client_version` / client version gate in config + maintenance of API `v1` (rg `client_version`); no multi-logic-version serialization scheme (n/a…* — [source](https://docs.metaplay.io/feature-cookbooks/game-updates-and-versioning/logic-versions)
- 🟡 **Metaplay** · Rolling out updates playbook: deploy server supporting both versions → raise active range → ship client → drop old; maintenance mode with scheduled window and player-facing message; store review timing — *WASM module canary/promote/rollback for server logic; NO maintenance mode (`rg -i maintenance` → none)* — [source](https://docs.metaplay.io/feature-cookbooks/game-updates-and-versioning/rolling-out-updates)
- 🟡 **Metaplay** · Secrets: `metaplay secrets create/update/show/list/delete`, `user-` prefix, multi-entry, `kube-secret://`, `aws-sm://` (self-hosted), `file://`, `unsafe://`; local server auto-loads cloud secrets — *config via env/TOML + K8s secrets in Helm; no secret URL resolver or CLI* — [source](https://docs.metaplay.io/cloud-deployments/how-to-guides/persisting-secrets)
- 🟡 **Nakama** · Builders: connect GitHub/GitLab/Bitbucket (OAuth or PAT, self-hosted Git on request), compile Go (`main.go`, vendored deps) / pre-bundled JS / Lua against a chosen Nakama version into a container image; build triggers from dashboard or API; auto-deploy to target instances; Nakama+Hiro layout (`definitions/`, `lib/`) — Managed — *musubi: `musubi deploy` manifest + `module publish/canary/promote/rollback` from CI (README CLI rows 33–35) — module pipeline exists but no hosted build servic…* — [source](https://heroiclabs.com/docs/heroic-cloud/concepts/titles/builders/)
- 🟡 **Nakama** · Satori deployments (managed LiveOps: events, messaging, live events, A/B tests, segmentation, analytics, flags; managed scaling; billable events) and Hiro — separate paid products integrated with Nakama console — Paid — *Zanshin (open-source Satori analog) integration for flags/live events/analytics; no A/B experiments/segmentation engine inside musubi itself* — [source](https://heroiclabs.com/docs/heroic-cloud/concepts/titles/satori-deployments/)
- 🟡 **Photon** · AppVersion isolation — AppVersion string segments players of same AppId into separate matchmaking pools — *musubi `appversion` appears in 3 files (matchmaking filters); no first-class pool isolation per build* — [source](https://doc.photonengine.com/pun/current/getting-started/feature-overview)
- 🟡 **Photon** · Auto-scaling & self-healing cloud — Premium/Enterprise auto-scale game servers; Photon operates and heals — *musubi fleet autoscaling/scale-to-zero (README fleets) but you operate the infra* — [source](https://doc.photonengine.com/photon/current/photon-cloud)
- 🟡 **Photon** · Managed regions — 16 gaming regions (asia, au, cae, cn, eu, hk, in, jp, za, sa, kr, tr, uae, us, usw, ussc); Chat regions asia/eu/us/cn; Voice/Video fewer; e… — *musubi georouting + fleets per region (README fleets/georouting) but you provision the regions yourself; no managed PoPs* — [source](https://doc.photonengine.com/realtime/current/connection-and-authentication/regions)
- 🟡 **Pragma** · Discord provisional accounts — Discord Activities users get provisional accounts upgradeable later. — *device/guest accounts can be upgraded by linking (account merge), but no Discord Activities-specific flow.* — [source](https://pragma.gg/docs/2026.3.0/concepts/social/how-to-guides/discord-provisional-accounts)
- 🟡 **Rivet** · Testing: `setupTest` (Vitest) isolated registry, fake timers for schedules — docs — *`musubi-sim-sdk` simulated clients + integration test suites; no module unit-test harness for guest authors* — [source](https://rivet.dev/actors/docs/testing/)
- 🟡 **SpacetimeDB** · Tutorials / sample games — Blackholio (Unity, Godot), chat apps (Rust/C#/TS/C++), quickstarts per language/framework — *README + docs; no sample game clients*
- 🟡 **SpacetimeDB** · `spacetime build` with lint — `--lint-dir` prints lints — *`musubi module` subcommands; `musubi doctor` checks deployment, not module lints*
- 🟡 **SpacetimeDB** · `spacetime call` / `describe` — Invoke reducer/procedure; describe tables/views/reducers/procedures/routes/env/types (`--json`); unstable — *`GET /openapi.json`, `GET /v1/admin/idl` describe the API; RPC callable via curl, no CLI verb*
- 🟡 **SpacetimeDB** · `spacetime init` project templates — Templates: basic-ts/cs/rs/cpp, react-ts, chat-console-rs/cs, chat-react-ts; `--template` lists — *`musubi dev` + `musubi config`; no project/module scaffolding templates*
- 🟡 **SpacetimeDB** · `spacetime logs` — `-n`, `-f`, `--format text` / `--format json`, `--level`, `--level-exact` — *server `tracing` logs to stdout; no CLI to tail remote module logs*
- 🟡 **Steamworks** · Playtest program — child AppID, limited/open signup, country gating, deactivate — *login queue + LiveOps flags can gate access; no signup funnel* — [source](https://partner.steamgames.com/doc/features/playtest)
- 🟡 **UGS LiveOps** · Project environments across all services (≤25), Environment selector, Environments API, build/runtime behaviour — Core SDK 1.16 toolbar environment button — *One cluster per environment (`node.environment`), multi-project per cluster; no env switch API* — [source](https://docs.unity.com/en-us/services/environments-api)
- 🟡 **UGS LiveOps** · Samples / use-case repository (seasonal events, daily rewards, battle pass, coupons, etc. built on Cloud Code) — GitHub use-case samples — *Features are native; few end-to-end sample games (`examples/`)* — [source](https://docs.unity.com/en-us/cloud-code/scripts/use-cases)
- 🟡 **UGS Multiplayer** · Boss Room / samples, casual co-op quickstart — sample projects via Multiplayer Center — *README examples + `examples/` only; no engine sample game.* — [source](https://docs.unity.com/en-us/multiplayer/quickstarts/casual-co-op-quickstart)

**P2 · console/admin** — owner: new `console/` SPA over `/v1/admin/*` (operator SSO + roles)

- 🟡 **Beamable** · Player admin page — Search by playerId/device/email; view/edit inventory (add currency/items), stats, tournaments, cloud data, credentials; ban — *All admin APIs exist (`/v1/admin/accounts*`, inventory, storage); no web UI* — [source](https://docs.beamable.com/docs/portal-players)
- 🟡 **Beamable** · Team user administration & roles — Admin > User Administration; invite by email (password-reset onboarding, code valid 1h); roles none/tester/developer/admin; realm-scoped ro… — *Operator RBAC roles + OIDC SSO + per-project scoping (README admin rows); no realm-hierarchy inheritance, no UI invite flow* — [source](https://docs.beamable.com/docs/portal-granting-realm-scoped-developer-permissions)
- 🟡 **Colyseus** · `@colyseus/testing`: boot server from `app.config.ts`, `createRoom`, `connectTo`, `waitForMessage`, `waitForNextMessage`, `waitForNextPatch`, `waitForNextTimestep`, `colyseus.sdk.*`, `colyseus.http.get/post/...`, `cleanup()` — OSS — *Rust integration-test harness in repo (CI full suite vs Postgres) + `musubi dev` fixtures; no published test-kit for module authors to drive a match from a test* — [source](https://docs.colyseus.io/tools/unit-testing)
- 🟡 **Edgegap** · Analytics (PAYG) — vCPU/RAM/network near-real-time, container count 7 days, OOM insights, deployment analytics, matchmaking analytics (Enterprise) — *Prometheus/OTel export + usage/demand JSON; operator brings Grafana* — [source](https://docs.edgegap.com/docs/release-notes/archive)
- 🟡 **Edgegap** · Notifications — Private fleet schedules, uncached deployments, rate-limit alerts, auto-cleaned Error deployments — *Audit log + webhooks; no operator notification center* — [source](https://docs.edgegap.com/docs/release-notes/archive)
- 🟡 **Edgegap** · Testing & Releases scheduler — Schedule releases/playtests/demos from dashboard (capacity planning) — *`ScalePolicy.schedule[{days,from_hour,to_hour,buffer}]` scales by schedule; no event planner UI* — [source](https://docs.edgegap.com/docs/release-notes/archive)
- 🟡 **EOS** · Developer Authentication Tool (local credential server) — SDK/Tools — *see Auth row* — [source](https://dev.epicgames.com/docs/epic-online-services/accounts-and-social/eos-epic-account-services/developer-authentication-tool)
- 🟡 **Hathora** · Customer references — Stormgate (1 M CCU test), Splitgate 2, Predecessor, Wildcard Alliance — *no public production references* — [source](https://blog.hathora.dev/hathora-is-joining-fireworks-ai/)
- 🟡 **Hathora** · Multi-cloud / multi-provider scheduling — Global scheduler bin-packs by proximity, availability, customer constraints; committed nodes before on-demand — *`allocate_routed` by region/pool; no cost-aware node bin packing* — [source](https://hathora.dev/docs/how-hathora-works/architecture)
- 🟡 **Hathora** · Security patching via draining — Drain after 6 h / 24 h to roll hardware/OS patches — *node drain exists; game-server hosts patched by operator* — [source](https://blog.hathora.dev/changelogs-august-2023/)
- 🟡 **LootLocker** · Organization users with roles Admin / Developer (no Game Settings) / Moderator (leaderboards, UGC, reports, profiles only); invite by email; default full access until role assigned — *Admin keys with roles + operator SSO (`/v1/admin/sso`, `0094_operator_sso.sql`, parity.md RBAC); no org/user invite model or web UI* — [source](https://docs.lootlocker.com/admin/organization-settings)
- 🟡 **LootLocker** · Send individual notification from console (type, priority, JSON content, properties), read status — *`POST /v1/server/notifications` with code/content; no priority field (rg priority in notifications = 0); no UI* — [source](https://docs.lootlocker.com/players/managing-players/how-to/send-player-notification)
- 🟡 **LootLocker** · Wallet Modifiers / Currency Multipliers (Jun 2025): per-player multipliers applied to currency grants; Bulk balance operations; Bulk upsert metadata; Bulk Edit Filters; Bulk Create Leaderboards — *Reward modifiers (`/v1/rewards/modifiers`, README reward modifiers: multipliers/time-boxed) cover grant multipliers; batch wallet/metadata admin ops not eviden…* — [source](https://ref.lootlocker.com/admin)
- 🟡 **Metaplay** · Debug production player locally: export from cloud → import to local → Reconnect Devices to attach own login — *export/import possible; no identity re-attach tool* — [source](https://docs.metaplay.io/operating-a-live-game/customer-support/debugging-player-state)
- 🟡 **Metaplay** · Environment page: node list with workload/drain status, database shard info, cluster status, deployment info — *`GET /v1/admin/cluster/nodes` + health endpoints (README cluster row); no UI* — [source](https://docs.metaplay.io/game-server-programming/how-to-guides/dynamic-scaling)
- 🟡 **Metaplay** · Event stream viewer (player/guild event logs per entity) — *server event bus (`/v1/server/events`) streams to Zanshin/webhooks; no per-entity history query* — [source](https://docs.metaplay.io/liveops-dashboard/how-to-guides/viewing-event-streams)
- 🟡 **Metaplay** · Event streams: Latest Player Events + Latest Audit Log Events per player, filter/highlight, keyword presets — *audit log per account via `/v1/admin/audit?account_id`; no player event log* — [source](https://docs.metaplay.io/liveops-dashboard/how-to-guides/viewing-event-streams)
- 🟡 **Metaplay** · Game config management page (view, build history, diff, publish, rollback) — *content bundle versions/diff/rollback exist via API (`/v1/admin/content/*`), no UI* — [source](https://docs.metaplay.io/liveops-dashboard/introduction-to-the-liveops-dashboard)
- 🟡 **Metaplay** · Guild pages: guild list/search, guild details, member list, event log, admin actions (edit, kick), export guild — *admin group routes exist, no UI* — [source](https://docs.metaplay.io/feature-cookbooks/guilds/preview-guild-framework)
- 🟡 **Metaplay** · Reconnect Devices tool: move login methods between accounts, per-method checkbox, preview, audit on both accounts, undo via audit payload — *musubi has link/unlink identities + account merge (`/v1/account/link/*`, `/v1/account/merge`), but no CS-driven move of a single login method with undo trail* — [source](https://docs.metaplay.io/operating-a-live-game/customer-support/recovering-lost-player-accounts)
- 🟡 **Nakama** · Groups page (search by id/name, edit members/roles), Storage browser (collection/key/user filter, `key%` prefix, create/edit/delete, import CSV/JSON, export values), Leaderboards+Tournaments viewer (records, delete), Chat messages (search by channel/user, delete), Matches (live state, presences, label), Payments (purchases/subscriptions filters), Notifications (view, send) — OSS — *all backed by admin routes in musubi (`/v1/admin/storage`, `/v1/admin/chat/*`, leaderboards server routes, `/v1/server/notifications`) but no UI* — [source](https://heroiclabs.com/docs/nakama/getting-started/console/)
- 🟡 **Photon** · Counter analytics — CCU, peak CCU, rooms, CCU/room, msg/s/room, bandwidth/s, per region, time spans; free apps limited windows — *Prometheus metrics + OpenTelemetry (README observability) but no built-in charts UI* — [source](https://doc.photonengine.com/realtime/current/reference/counter-analytics)
- 🟡 **Photon** · Dashboard config keys — FusionSessionCreationControlEnable/ServerId (only server UserID may create sessions), FusionDummyTraffic (keepalive), FusionSharedTickEnabl… — *server-only match creation enforced by server keys; tick config in match config; no dashboard* — [source](https://doc.photonengine.com/fusion/current/manual/advanced/dashboard-config)
- 🟡 **Photon** · Plugin/webhook key-value config — per AppId key/values ≤1024 chars, takes effect ≤1 min — *musubi `config` service (migrations 0113 configs, content environments) but no per-app webhook config UI* — [source](https://doc.photonengine.com/fusion/current/manual/advanced/webhooks)
- 🟡 **Pragma** · Portal: Configuration viewer & Feature Toggles — Read effective config; toggle portal features. — *`GET /v1/admin/config` (redacted) exists; no toggles UI.* — [source](https://pragma.gg/docs/2026.3.0/concepts/portal/operator-portal/social-operator-portal)
- 🟡 **SpacetimeDB** · Team collaboration / orgs — Collaborators per DB (5 Free/10 Pro/unlimited Team), `--parent` inherited permissions, organizations with org permissions, 10 org users the… — *operator RBAC + SSO (`0094`) for API; no org/project hierarchy*

**P2 · compliance** — owner: musubi-server compliance.rs/retention.rs (`/v1/account/export`, age gate, `[retention]`)

- 🟡 **AccelByte** · Age restriction: global min age + per-country overrides for account creation — Private Cloud only — *age gate brackets child/teen/adult with feature closures + parental consent (`[age_gate]`); no per-country minimum-age table* — [source](https://docs.accelbyte.io/gaming-services/modules/foundations/legal/manage-age-restriction/)
- 🟡 **Colyseus** · WebSocket close code contract: 4000 CONSENTED, 4001 SERVER_SHUTDOWN, 4002 WITH_ERROR, 4003 FAILED_TO_RECONNECT, 4010 MAY_TRY_RECONNECT, 4011–4999 app range — *errors carry codes/`trace_id` in `error` envelopes; close reasons like `unavailable: slow consumer`; no documented app-reserved close-code range* — [source](https://docs.colyseus.io/room)
- 🟡 **GameLift** · Cost optimization guidance — Graviton, Spot, Linux, scale-to-zero, queue COST priority, compression, cost allocation tags, Cost Explorer, Budgets — *vCPU-hour usage per build/region (`GET /v1/server/fleets/{id}/usage`); no cost-aware placement* — [source](https://docs.aws.amazon.com/gamelift/latest/developerguide/gamelift-pricing-cost-optimization.html)
- 🟡 **GameLift** · Resource tags — `TagResource` for cost allocation / ABAC on fleets, builds, queues, matchmakers — *fleet labels (`POST /v1/gameserver/label`), no cost tagging* — [source](https://docs.aws.amazon.com/gamelift/latest/apireference/API_TagResource.html)
- 🟡 **Hathora** · Scale claims — 1 M CCU load test (Stormgate); Splitgate 2, Predecessor in production; "thousands of cores" — *`musubi bench`/soak numbers in README (1 M WS pings/s on one node) but no public production title* — [source](https://blog.hathora.dev/hathora-is-joining-fireworks-ai/)
- 🟡 **Hathora** · Usage billing units — vCPU-seconds + egress bytes; billing starts at port bind; monthly invoice, credit first, 30 days to pay; $500 starter credit (2023) — *`GET …/fleets/{id}/usage` vCPU-hours per build for chargeback; no egress metering* — [source](https://hathora.dev/docs/pricing-billing)
- 🟡 **LootLocker** · Error schema: request_id, trace_id, code, doc_url, message, param; documented error codes — *Stable error codes in JSON responses; no request/trace id or doc_url in error bodies (rg trace_id limited to otel)* — [source](https://docs.lootlocker.com/the-basics/support/error-codes)
- 🟡 **LootLocker** · SDK Error Reports: client reports failed requests (user_description, SDK version, trace_id, redacted request/response bodies + headers, retry_attempts, client/server timestamps, duration); console filters (player, UID, endpoint, status, open/resolved, date), CSV export, resolve/unresolve/delete — 7-day retention, 5,000 reports/game — *rg -i 'error report' = 0; musubi has server-side request recording/replay (`musubi record`) but no client-submitted error reports* *(verdict changed, §5.3)* — [source](https://docs.lootlocker.com/the-basics/support/error-reports/overview)
- 🟡 **Metaplay** · Custom audit events from game-specific admin endpoints — `AuditLog` helpers in AdminApi controllers — *WASM modules cannot append custom audit entries; audit is fixed to built-in admin routes* — [source](https://docs.metaplay.io/liveops-dashboard/how-to-guides/working-with-the-liveops-dashboard-http-api)
- 🟡 **Rivet** · FoundationDB storage driver, multi-tenancy layer — Enterprise — *Postgres + Raft cluster; multi-region clustering free (README L232), but no FDB-scale backend* — [source](https://rivet.dev/enterprise/)
- 🟡 **Rivet** · SQLite backup + PITR, actor forking — Enterprise — *match snapshots/journal/replay (`GET …/replay`), per-account backup/restore; no PITR or fork of a running actor (`rg -i fork` = 0)* — [source](https://rivet.dev/enterprise/)

## 4. Where musubi leads (consolidated)

Every report has a "where musubi is ahead" table (row counts in §1.1, 436 rows in total). Deduplicated across the 22 reports:

| lead | what musubi has (evidence) | reports that list it |
|---|---|---|
| **Licensing & hosting model** | Apache-2.0 single binary on Postgres, self-host anywhere (Docker, Helm, Terraform for k8s/Hetzner/OVH), multi-project, no CCU/MAU caps, no revenue share — vs SaaS-only (AccelByte, PlayFab, Beamable, brainCloud, LootLocker, UGS, EOS, Steamworks, GameLift, Edgegap, Hathora), per-seat/commercial licences (Metaplay, Pragma, Hiro, coherence 3 % rev share), BSL with 1 production instance (SpacetimeDB) or Enterprise-only clustering (Nakama). | all 22 |
| **Server meshing / replication layer** | Grid + quadtree cells with cost-based split/merge, simulator leases, entity authority handover with hysteresis, ghosts, group handover, interest management with byte budgets, cross-cell atomic transactions (2PC), lag-compensation rewind, world snapshots/journal/time-machine, cold cells, multi-region placement, placement strategies incl. WASM hooks — none of the BaaS/hosting vendors offer meshing; among meshing vendors musubi uniquely combines durable persistence, cross-cell atomicity and time-machine (`musubi-mesh`, `/v1/server/worlds/*`). | 21 (all but coherence/Hadean partially) |
| **Server-authoritative match actors** | WASM match modules at 1–60 Hz with snapshots + journal failover across nodes, deterministic rollback inputs (`MatchInput/Ack/TickHash`), typed IDL state deltas with interest, spectators, match recording/replay, end-of-match results that settle rating/rewards/quests/XP in one call (`musubi-actor`, `/v1/server/matches/{id}/results`). | 19 |
| **Realtime transports** | WebSocket, QUIC, WebTransport, WebRTC data channels and raw UDP behind one envelope, four channel classes, lz4 per-frame compression, resumable sessions with sequence replay, relayed matches ≤64 players with host migration (`musubi-transport`). | 20 |
| **Dedicated-server fleets without Kubernetes lock-in** | Nine providers (local, docker, kubernetes, agones, firecracker with snapshots/warm pool, ssh bare-metal, webhook, edgegap, gamelift), pools + georouting, buffer/min/max + weekday/hour schedules + EWMA forecast, scale-to-zero, build canary/promote/rollback, vCPU-hour usage, connect tokens, `musubi fleet push` / `musubi deploy` manifests (`musubi-fleet`). | 18 |
| **Clustering in the OSS core** | Node registry + heartbeats, SWIM + Raft, Postgres LISTEN/NOTIFY bus, replicated chat/presence/notifications/matchmaker tickets, match/party routing across nodes, node drain, failover of worlds and matches (`musubi-cluster`). | 16 |
| **Identity breadth** | Device, email+password (lockout, CAPTCHA hook), magic links, passkeys/WebAuthn, TOTP MFA + recovery codes, Steam, Apple, Game Center, Google, Facebook, Xbox XSTS, PSN, Nintendo NSA, generic OIDC (Discord/Twitch/Epic/Keycloak), custom server-minted sessions, link/unlink/merge, refresh-token families with reuse detection, session list/device sign-out, session cap, login queue, suspicious-login events, operator SSO, JWT key rotation + JWKS (`musubi-auth`). | 17 |
| **Matchmaker extras** | Config-driven queues with teams/roles/MMR + ping expansion, crossplay groups, ready check with dodge penalties, avoid lists, no-repeat opponents, parties as units, backfill, built-in Elo/Glicko-2/OpenSkill with seasons and decay, queue simulation on virtual time, p50/p95/p99 wait stats (`musubi-matchmaker`). | 15 |
| **Economy depth** | Currencies/wallets/append-only ledger, items with versions + rollback, inventory limits, store with per-platform listings/bundles/rotation/discounts/coupons/audiences, IAP for Apple/Google/Steam/PSN/Xbox/Nintendo/Epic/Huawei/Samsung + subscriptions, entitlements sync, reward tables/rolls/modifiers, grant limits, crafting, trading, marketplace with order book, fraud heuristics, group treasury/inventory (`musubi-economy`). | 14 |
| **Progression & meta** | Progression tracks, skill-tree/timed-unlock graphs, quests with rerolls, seasons/battle pass, streaks, energy, achievements, player statistics with scopes, group XP/challenges/wars, alliances, bracket tournaments with prizes and team tournaments, bucketed leaderboards + leagues. | 13 |
| **Chat depth** | Room/group/direct channels, history, threads, reactions, edits, mentions, typing, unread, attachments, slow mode, mute/shadow-ban, word filters, automod + translation webhooks, reports and admin moderation routes (`musubi-chat`). | 12 |
| **Durable workflows / sagas and timers** | Module-driven workflows with persisted state, retries, signals, compensation, cluster-wide lease execution; timers and schedules (`/v1/server/workflows*`, `/v1/admin/runtime/timers*`). | 12 |
| **In-process WASM modules with safe rollout** | Fuel/memory/deadline limits, hot reload, canary by percent/pinned accounts, promote/rollback, typed IDL with generated realtime types, before/after hooks, webhooks with deliveries/redeliver — vs out-of-process billed Extend apps (AccelByte), Cloud Code (UGS), microservices (Beamable). | 11 |
| **Storage semantics** | JSON Merge Patch + RFC 6902 ops with `test` guards, OCC versions, aggregates, full-text (`$q`), collection schemas/migrations/indexes, live collection subscriptions, server-side atomic multi-op transactions (`POST /v1/server/tx`). | 10 |
| **Compliance primitives** | GDPR export/erase per account, retention per table, COPPA age brackets with per-bracket feature closures, parental consent tokens, audit log, operator roles/scoped keys — most hosting vendors have none. | 11 |
| **Developer tools** | `musubi bench` (http/storage/ws/chat/match/world), `musubi soak` chaos stand, fixtures, request recording/replay, on-demand CPU flamegraphs/pprof, `musubi doctor`, `export/import`, PlayFab/UGS migration importer, measured benchmark table in README. | 10 |
| **Push & notifications** | FCM/APNs/Web Push/HMS with templates, silent pushes, deep links, per-recipient local-time scheduling; notification TTLs; mail with reward claim. | 8 |
| **Open API surface** | 578 routes in `GET /openapi.json` (OpenAPI 3.1), typed IDL, documented realtime envelopes — vs no public OpenAPI/protos at Photon, Steamworks, SpacetimeDB, EOS, Pragma, LootLocker. |  7 |

Per-report nuance: Agones/Open Match, Edgegap, Hathora, GameLift, Rivet, SpacetimeDB, Photon and the meshing vendors concede the *entire* game-services layer; the BaaS vendors (PlayFab, Beamable, brainCloud, LootLocker, Metaplay, UGS LiveOps, AccelByte, Hiro/Satori, Nakama) concede realtime/meshing/hosting and usually console logins, passkeys/MFA, trading/marketplace and workflows.

## 5. Method and coverage

### 5.1 How the merge was done

1. All 22 report files were parsed programmatically (`## Feature inventory` tables; sub-vendor tables in the meshing report tagged with their vendor) → 4516 rows, statuses normalized to have/partial/missing/n/a. Per-report totals reproduce the counts each report stated.
2. Raw areas (~400 distinct labels) were mapped onto the 20 target areas by area-label rules plus feature-text rules.
3. A hand-authored taxonomy of 298 canonical features (regex per feature) merged rows across competitors inside their area (and across adjacent areas for portable features); 2812 rows merged into 264 groups, 1704 rows kept as single-competitor features so nothing is dropped.
4. musubi's aggregated status per group = majority of row verdicts (missing wins ties against partial/have; all-n/a groups show —). Priorities follow the rule in §3 with manual pins for SDKs/console/analytics (P0) and niche items (P2).
5. Doubtful verdicts were checked against the musubi checkout (`rg` over crates/, README, docs, deploy, sdk) — see §5.3.

### 5.2 Coverage per competitor

| competitor | pages scanned | coverage notes and gaps |
|---|---|---|
| **AccelByte** | 1,399 | 1,400 sitemap URLs fetched (0 failures); /api-explorer/ JS-only shell covered via 24 OpenAPI specs (2,318 ops). Not covered: price lists, login-gated console partner packages. |
| **Agones/OM** | 167 | 93 Agones + 74 Open Match docs pages via docs repos; full Agones CHANGELOG (v0.5.0–v1.61.0), protos, features.go, Helm values; OM1/OM2 protos. 28 n/a rows are k8s/GCP-internal mechanics. |
| **Beamable** | 467 | docs.beamable.com rate-limited (429) for 96/305 legacy pages and 348/350 API-reference pages; fallback to github.com/beamable/docs versioned branches (1,460 md) + web.archive.org (57 pages). Pricing page and Portal UI not accessed. |
| **brainCloud** | 1,720 | All 1,607 docs pages + braincloud-docs repo + 65 llms/*.md; 84 release posts via RSS + 18 AnnounceKit posts. SLA uptime table JS-rendered, not captured. |
| **Colyseus** | 142 | All 142 sitemap pages via colyseus/docs repo; CHANGELOGs 0.17.8→0.18.9. Not accessible: notes older than 0.17.7, Cloud per-plan prices, Cloud dashboard. |
| **Edgegap** | 81 | 74 docs pages + swagger.json (72 ops) + v1/v2 OpenAPI YAML + 4 public repos. Marketing pricing page not scanned; no SOC2/GDPR/COPPA docs exist. |
| **EOS** | 228 | Section root 403 to non-browser clients; 228 pages read via __NEXT_DATA__ MDX; release notes from web.archive.org (1.13→1.18.1.2) + live What's New (1.19.x). Console SDK docs approval-gated; ~1,500 api-ref pages and Developer Portal UI not walked. |
| **GameLift** | 628 | All 628 pages fetched directly; GitHub org listing rate-limited (repo list from HTML). FleetIQ dev guide and pricing page outside roots; 68 n/a rows are AWS billing/infra + GameLift Streams. |
| **Hathora** | 116 | Entire scan via web.archive.org (72 docs + 44 blog/changelog pages), archived swagger.json (115 ops), npm cloud-sdk-typescript 2.25.6, Go SDK. Console UI, Unity/Godot plugin sources, CLI repo not accessible. |
| **LootLocker** | 1,198 | All 267 docs pages + pricing/platform/9 changelog pages (219 entries) + 916 of 1,048 ref.lootlocker.com pages (132 returned Apidog JS error shell; names only). Unity/Unreal reference variants, OpenAPI export not available. No SOC2/ISO/COPPA/SLA published. |
| **Meshing vendors** | 629 | coherence 187 docs + 17 release notes; Hadean 81 site/blog + 3 archived pages; SpatialOS 313 archived documentation.improbable.io pages (~1 req/s; remaining 1,046 were api-ref stubs) + 8 GitHub READMEs; Star Citizen 11 wiki/patch pages; Flax 7. docs.hadean.com never archived; RSI comm-link blocked; SpatialOS shut down 2021 (historical rows). |
| **Metaplay** | 333 | 335 sitemap URLs; VitePress shell → bodies extracted from JS chunks (333 with content, 2 empty). Pricing from metaplay.io/pricing; CLI from public github.com/metaplay/cli. SDK/sample repos require Portal account. |
| **Hiro/Satori** | 131 | 44 Hiro + 44 Satori concept pages, 12 Heroic Cloud, guides/SDK/pricing/blog + 8 files from public heroiclabs/hiro repo (CHANGELOG 1.0.0–1.36.0, hiro.proto, OpenAPI). 110 per-language SDK reference pages not read individually; Hiro server impl closed; Satori API refs JS-rendered. |
| **Nakama** | 167 | 187 pages fetched (177 docs + 10 nav), 167 read; runtime surface from nakama-common runtime.go (NakamaModule 149 fns, Initializer 196 fns); full 3.18–3.41 changelog. Two Hugo shortcode tables render client-side (partial). |
| **Photon** | 124 | 1,137 English URLs; 1,098 downloaded; 38 pages 401/403 'No Access' (enterprise private cloud, clusters/sharding, plugin upload, consoles); 124 read in full, all 1,099 indexed at heading level (6,502 headings). No public docs repo / OpenAPI / protos; web.archive.org 404 for gated URLs. |
| **PlayFab** | 2,772 | MicrosoftDocs/playfab-docs repo (2,781 md, HEAD 2026-01-13) processed in full; ~1,900 generated API-reference pages summarized; PlayFab/API_Specs Swagger (762 paths). Per-meter $ rates not in docs; console-platform gated SDK docs not read; some source URLs derived from repo paths. |
| **Pragma** | 1,049 | No sitemap; BFS crawl of pragma.gg/docs/2026.3.0 (1,049 pages incl. 692 Kotlin API-ref pages, 169 concept, 36 tutorials, 11 tech-blog, all 12 release-note versions). Engine source customer-only (no protos/OpenAPI); console commerce + Epic/Discord guides CSM-only; no pricing/SLA/SOC2/COPPA published. |
| **Rivet** | 614 | rivet.dev sitemap-0.xml (557 URLs, 532 fetched, 0 errors); v1 game-backend docs reconstructed from repo .mdx (65 files) + 17 Wayback snapshots (Wayback refused 246/265). No CHANGELOG.md (116 GitHub releases + 54 changelog posts). 38 n/a = Cloud billing/BYOC/AI-agent products. |
| **SpacetimeDB** | 115 | 115 current docs pages from clockworklabs/SpacetimeDB docs/docs (sitemap 272 URLs incl. legacy/duplicate slugs) + all 66 GitHub release notes + LICENSE (BSL 1.1) + pricing page. No OpenAPI/protobuf published. |
| **Steamworks** | 134 | 134 partner.steamgames.com pages (46 features, 33 webapi = 172 methods, 32 native api, overview pages) + 375 Steamworks Development announcements (SDK 1.61→1.65). sitemap.xml returns HTML; partner-login-only SDK release notes and App Admin pages inaccessible; no public repo/OpenAPI. |
| **UGS LiveOps** | 706 | 652 docs pages via .md endpoints + 42 OpenAPI specs + release notes (Jul 2022–Jul 2026) + 11 package changelogs; 429 handled with retries. Not fetched: unity.com pricing tables, Cloud Code JS SDK portal, samples repo, 1,051 repetitive services-cli pages; 115 Unity Cloud Create pages excluded. |
| **UGS Multiplayer** | 1,123 | web.archive.org unreachable from VM; Multiplay pages replaced by support.multiplay.dev (174 pages). docs.unity.com serialized at ~1 req/s; discussions.unity.com blocked; pricing from search snapshot. NGO 3.1 (109 pages), N4E 7.1 (66) manuals; 17 package changelogs. Multiplay deprecated at Unity 2026-04-01. |

Known blind spots common to several reports: console-platform SDK docs (NDA-gated at EOS, PlayFab, Nakama, Photon, Pragma); vendor pricing pages outside docs hosts; SaaS dashboards behind login (no UI walked for any vendor); SOC 2/ISO attestations not published by most vendors. Rows marked n/a (vendor billing, cloud-internal mechanics, pixel streaming) are kept in the CSV but excluded from the backlog.

### 5.3 Verdict changes made during the merge

The repository was grepped before changing any verdict. Changed rows (also flagged in the CSV `musubi_evidence` column):

| competitor | feature | old → new | evidence |
|---|---|---|---|
| PlayFab | LoginWithFacebook (access token | missing → **have** | musubi has a dedicated Facebook provider: `POST /v1/auth/facebook`, `POST /v1/account/link/facebook` (ROUTES in openapi.rs; `[social.facebook]` in musubi-core/config.rs; covered in tests/api.rs). The report's grep missed it. |
| Nakama | Huawei AppGallery validation | missing → **have** | `POST /v1/iap/huawei` exists (openapi.rs ROUTES; musubi-economy/src/iap.rs:119 provider list `apple\|google\|steam\|psn\|xbox\|nintendo\|epic\|huawei\|samsung`; README L335). parity.md was stale. |
| Nakama | Samsung Galaxy Store validation | missing → **have** | `POST /v1/iap/samsung` exists (openapi.rs ROUTES; iap.rs provider list; README L335 'Huawei AppGallery and Samsung Galaxy Store work like Google Play'). |
| Photon | Epic EOS auth | missing → **partial** | Epic sign-in is supported through the generic OIDC provider (`[social.oidc.<name>]`, README L204 names Epic explicitly) and Epic Ecom entitlements via `[iap.epic]`; there is no dedicated EOS verifier, so partial rather than missing. |
| Colyseus | Quantized floats | missing → **partial** | musubi-mesh quantizes world position deltas (`fn quantize` in crates/musubi-mesh/src/lib.rs:1898, test `delta_filter_and_quantization`); there is no per-field quantization in the IDL/match-state codec, so partial. |
| LootLocker | SDK Error Reports | missing → **partial** | musubi-server/src/telemetry.rs returns `X-Trace-Id` and `trace_id` in error bodies; no doc_url / redacted payload console, so partial. |

Checked and left as reported (confirmed absent in the checkout at `552e2f1`): country deny list, device/HWID bans, GeoIP/MaxMind, turn-based/async matches, device-code login, SQLite dev mode, admin account notes, localization pipeline, legal/EULA consent versioning, hitbox lag compensation, STUN/TURN/ICE, octree partitioning (`musubi-mesh/src/lib.rs` "3D/octree … later"), rooms-per-process packing, minimum client version, MCP server, A/B experiments (only Zanshin wiring in `musubi-integrations`), referral codes, rewarded-video S2S, Stripe/Xsolla/PayPal, Oculus/Viveport/Kongregate/GPGS providers, platform account-closure webhooks, sanctions/appeals, maintenance mode, Grafana dashboards, ClickHouse/BigQuery/Kafka sinks, game-server stdout capture, health policies, sidecars, Vault/KMS secret refs, docker-compose, UPGRADING/CHANGELOG, SAML.

### 5.4 Files

- `musubi-competitive-analysis.md` — this document.
- `musubi-competitive-matrix.csv` — flat `area,feature,competitor,competitor_detail,source_url,musubi_status,musubi_evidence,priority`, one row per (feature, competitor) pair from all reports (4516 rows; `area` is the normalized area, the original report area is appended to the feature as `[orig: …]`; `priority` is filled for missing/partial rows).
